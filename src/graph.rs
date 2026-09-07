use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};

use serde::{Deserialize, Serialize};
use tree_sitter::{Node, Parser, Tree};

use crate::index::SymbolIndex;
use crate::indexer::language::{Language, Symbol, SymbolId, SymbolKind};
use crate::path_policy::open_regular_file;

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
    let mut file = open_regular_file(sym.file.as_ref())
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

/// Name-to-candidate lookup built once per graph build. Replaces scanning
/// every indexed symbol for each source symbol (O(N²)) with O(1) lookups.
pub(crate) struct CandidateIndex {
    by_name: HashMap<(Language, String), Vec<SymbolId>>,
}

impl CandidateIndex {
    pub fn build(index: &SymbolIndex) -> Self {
        let mut by_name: HashMap<(Language, String), Vec<SymbolId>> = HashMap::new();
        for sym in index.symbols.values() {
            by_name
                .entry((sym.language.clone(), sym.name.clone()))
                .or_default()
                .push(sym.id.clone());
        }
        Self { by_name }
    }

    /// Same-language candidates for `name`, excluding `excluding_id`.
    fn lookup<'a>(
        &self,
        index: &'a SymbolIndex,
        language: Language,
        name: &str,
        excluding_id: &str,
    ) -> Vec<&'a Symbol> {
        self.by_name
            .get(&(language, name.to_string()))
            .into_iter()
            .flatten()
            .filter(|id| id.as_str() != excluding_id)
            .filter_map(|id| index.symbols.get(id))
            .collect()
    }
}

pub fn build_navigation_graph(index: &SymbolIndex) -> NavigationGraph {
    build_navigation_graph_with_source(index, |sym| read_symbol_source(sym, false))
}

/// Build the same navigation graph from a revision snapshot without reading the worktree.
pub(crate) fn build_navigation_graph_with_source(
    index: &SymbolIndex,
    source: impl Fn(&Symbol) -> anyhow::Result<String>,
) -> NavigationGraph {
    let mut graph = NavigationGraph {
        built: true,
        ..Default::default()
    };

    let candidates = CandidateIndex::build(index);
    for sym in index.symbols.values() {
        let source_text = match source(sym) {
            Ok(source) => source,
            Err(_) => continue,
        };
        for reference in scan_direct_references(index, sym, &source_text, &candidates) {
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
    scan_direct_references(index, sym, source_text, &CandidateIndex::build(index))
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
    candidates: &CandidateIndex,
) -> Vec<DirectReference> {
    let mut refs = Vec::new();
    let cap_generic_confidence = match sym.language {
        Language::Rust => {
            if let Some(mut rust_refs) =
                scan_rust_direct_references(index, sym, source_text, candidates)
            {
                refs.append(&mut rust_refs);
                Some(0.84)
            } else {
                None
            }
        }
        Language::Python => {
            if let Some(py_matches) = collect_python_call_matches(source_text) {
                for matched in py_matches {
                    resolve_ast_match(index, sym, &matched, candidates)
                        .into_iter()
                        .for_each(|r| refs.push(r));
                }
            }
            Some(0.84)
        }
        Language::TypeScript => {
            if let Some(ts_matches) = collect_typescript_call_matches(source_text) {
                for matched in ts_matches {
                    resolve_ast_match(index, sym, &matched, candidates)
                        .into_iter()
                        .for_each(|r| refs.push(r));
                }
            }
            Some(0.84)
        }
        _ => None,
    };

    refs.extend(scan_generic_direct_references(
        index,
        sym,
        source_text,
        cap_generic_confidence,
        candidates,
    ));
    sort_direct_references(&mut refs);
    refs
}

/// Resolve a single AST call match against the candidate index, producing
/// `DirectReference` entries for each same-language symbol that shares the name.
fn resolve_ast_match(
    index: &SymbolIndex,
    sym: &Symbol,
    matched: &CallMatch,
    candidates: &CandidateIndex,
) -> Vec<DirectReference> {
    let resolved = candidates.lookup(index, sym.language.clone(), &matched.name, &sym.id);
    if resolved.is_empty() {
        return vec![];
    }

    refs_from_resolved(&resolved, matched.evidence.as_str(), matched.confidence)
}

fn refs_from_resolved(
    resolved: &[&Symbol],
    evidence: &str,
    base_confidence: f32,
) -> Vec<DirectReference> {
    let ambiguous = resolved.len() > 1;
    let mut refs = Vec::new();
    for candidate in resolved {
        let confidence = if ambiguous {
            base_confidence.min(AMBIGUOUS_CONFIDENCE_CAP)
        } else {
            base_confidence
        };
        // Preserve the evidence line from the AST call site.
        refs.push(DirectReference {
            id: candidate.id.clone(),
            name: candidate.name.clone(),
            kind: candidate.kind.to_string(),
            file: candidate.file.to_string_lossy().replace('\\', "/"),
            line_start: candidate.line_start,
            evidence: evidence.chars().take(240).collect(),
            confidence,
        });
    }
    refs
}

/// Confidence cap for references whose target name is ambiguous (multiple
/// same-language candidates). Below the `Calls` thresholds in
/// `classify_relation`, so ambiguous name matches stay `References`.
const AMBIGUOUS_CONFIDENCE_CAP: f32 = 0.84;

/// Remove comments and string-literal contents from source, preserving
/// newlines so cleaned text stays line-aligned with the original.
///
/// Call evidence must never be inferred from text inside comments or
/// strings: a Python comment like `# authenticate_user()` previously
/// produced ~0.98-confidence `calls` edges.
fn strip_comments_and_strings(source: &str) -> String {
    #[derive(PartialEq)]
    enum State {
        Normal,
        LineComment,
        BlockComment,
        String(char),
        TripleString(char),
    }

    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut state = State::Normal;

    while let Some(c) = chars.next() {
        match state {
            State::Normal => match c {
                '"' | '\'' => {
                    let q = c;
                    if chars.peek() == Some(&q) {
                        chars.next();
                        if chars.peek() == Some(&q) {
                            chars.next();
                            state = State::TripleString(q);
                            out.push_str("   ");
                            continue;
                        }
                        // Empty string literal.
                        out.push(q);
                        out.push(q);
                        continue;
                    }
                    state = State::String(q);
                    out.push(q);
                }
                '/' if chars.peek() == Some(&'/') => {
                    chars.next();
                    state = State::LineComment;
                    out.push_str("  ");
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    state = State::BlockComment;
                    out.push_str("  ");
                }
                '#' => {
                    state = State::LineComment;
                    out.push(' ');
                }
                _ => out.push(c),
            },
            State::LineComment => {
                if c == '\n' {
                    state = State::Normal;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            State::BlockComment => {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = State::Normal;
                    out.push_str("  ");
                } else if c == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            State::String(q) => {
                if c == '\\' {
                    chars.next();
                    out.push_str("  ");
                } else if c == q || c == '\n' {
                    // Unterminated single-line strings (e.g. Rust lifetimes
                    // like &'a T) end at the newline rather than swallowing
                    // the rest of the file.
                    state = State::Normal;
                    out.push(c);
                } else {
                    out.push(' ');
                }
            }
            State::TripleString(q) => {
                if c == q && chars.peek() == Some(&q) {
                    chars.next();
                    if chars.peek() == Some(&q) {
                        chars.next();
                        state = State::Normal;
                        out.push_str("   ");
                        continue;
                    }
                    out.push(q);
                } else if c == '\n' {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
        }
    }
    out
}

fn scan_generic_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
    max_confidence: Option<f32>,
    candidates: &CandidateIndex,
) -> Vec<DirectReference> {
    // Comments and strings must not contribute call evidence: strip them
    // before extracting identifiers or searching for evidence lines.
    let cleaned = strip_comments_and_strings(source_text);
    let identifiers = extract_identifiers(&cleaned);

    identifiers
        .into_iter()
        // Candidate resolution is restricted to the referencing symbol's
        // language: a name match in another language is not a call.
        .flat_map(|name| candidates.lookup(index, sym.language.clone(), name, &sym.id))
        .map(|candidate| {
            // A name that resolves to multiple same-language candidates is
            // ambiguous: a name match cannot distinguish which one (if any)
            // is actually called.
            let ambiguous = candidates
                .lookup(index, sym.language.clone(), &candidate.name, &sym.id)
                .len()
                > 1;
            let (evidence, mut confidence) = reference_evidence(
                source_text,
                &cleaned,
                candidate.name.as_str(),
                &candidate.kind,
                ambiguous,
            );
            if call_shaped_unambiguous(
                &cleaned,
                candidate.name.as_str(),
                &candidate.kind,
                ambiguous,
            ) {
                // A direct `name(` occurrence in cleaned source is a genuine
                // call site even when the language AST scan cannot see it —
                // e.g. calls embedded in macro arguments such as `format!`,
                // whose token trees tree-sitter does not parse as expressions.
                confidence = confidence.max(0.98);
            } else if let Some(max_confidence) = max_confidence {
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

/// True when `name` appears in `cleaned` (comment- and string-stripped)
/// as a direct call: the character before the occurrence is not part of a
/// larger identifier and the character after it is `(`.
fn contains_call_pattern(cleaned: &str, name: &str) -> bool {
    let mut from = 0usize;
    while let Some(pos) = cleaned[from..].find(name) {
        let abs = from + pos;
        let after = abs + name.len();
        let prev_is_word = cleaned[..abs]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if !prev_is_word && cleaned[after..].starts_with('(') {
            return true;
        }
        from = after;
        if from >= cleaned.len() {
            break;
        }
    }
    false
}

/// A call-shaped occurrence of an unambiguous callable name is call-grade
/// evidence regardless of the line it appears on.
fn call_shaped_unambiguous(cleaned: &str, name: &str, kind: &SymbolKind, ambiguous: bool) -> bool {
    !ambiguous && is_callable_kind(kind) && contains_call_pattern(cleaned, name)
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
    // Keep the first (highest-priority) edge per target. Consecutive-dedup is
    // not enough: sorting by confidence can separate same-target edges when
    // their confidences differ (e.g. an AST-derived 0.99 call and a
    // call-shaped generic 0.98 for the same symbol).
    let mut seen: HashSet<String> = HashSet::new();
    bucket.retain(|edge| seen.insert(edge.symbol_id.clone()));
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

fn reference_evidence(
    source_text: &str,
    cleaned: &str,
    name: &str,
    kind: &SymbolKind,
    ambiguous: bool,
) -> (String, f32) {
    // Search the cleaned text (comments and string contents removed) so a
    // name that only appears in a comment or string produces no call-grade
    // evidence.
    for (line, cleaned_line) in source_text.lines().zip(cleaned.lines()) {
        if cleaned_line.contains(name) {
            let trimmed = cleaned_line.trim();
            let mut confidence: f32 = if trimmed.contains('(') {
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
            if ambiguous {
                confidence = confidence.min(AMBIGUOUS_CONFIDENCE_CAP);
            }
            let evidence = line.trim().chars().take(240).collect::<String>();
            return (evidence, confidence);
        }
    }

    let confidence = 0.72_f32.min(if ambiguous {
        AMBIGUOUS_CONFIDENCE_CAP
    } else {
        1.0
    });
    (
        format!("identifier `{name}` was extracted from the source text"),
        confidence,
    )
}

fn scan_rust_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
    candidates: &CandidateIndex,
) -> Option<Vec<DirectReference>> {
    let matches = collect_rust_call_matches(source_text)?;

    let mut refs = Vec::new();
    for matched in matches {
        // Resolve call targets within the same language only, and treat a
        // name shared by multiple Rust candidates as ambiguous.
        let resolved = candidates.lookup(index, Language::Rust, &matched.name, &sym.id);
        let ambiguous = resolved.len() > 1;
        refs.extend(resolved.into_iter().map(|candidate| {
            let confidence = if ambiguous {
                matched.confidence.min(AMBIGUOUS_CONFIDENCE_CAP)
            } else {
                matched.confidence
            };
            DirectReference {
                id: candidate.id.clone(),
                name: candidate.name.clone(),
                kind: candidate.kind.to_string(),
                file: candidate.file.to_string_lossy().replace('\\', "/"),
                line_start: candidate.line_start,
                evidence: matched.evidence.clone(),
                confidence,
            }
        }));
    }
    sort_direct_references(&mut refs);
    Some(refs)
}

// Shared call match type used by all language-specific AST extractors.
#[derive(Debug, Clone)]
pub(crate) struct CallMatch {
    pub name: String,
    pub evidence: String,
    pub confidence: f32,
}

// Alias for backward compatibility with existing Rust code.
type RustCallMatch = CallMatch;

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

// ── Python AST call extraction (Issue #85)──────────────────────────────
/// Collects all callable name matches from a Python source via tree-sitter.
/// Returns `None` if parsing fails; empty vec means no calls found.
fn collect_python_call_matches(source_text: &str) -> Option<Vec<CallMatch>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source_text, None)?;

    let mut matches = Vec::new();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        if node.kind() == "call" {
            // Python `call` nodes have a `function` field pointing to the
            // callable (identifier, attribute, subscript, etc.) and an
            // `arguments` field containing `argument_list`.
            let target = node.child_by_field_name("function");
            if let Some(name) = target.and_then(|t| python_callable_name(source_text, t)) {
                matches.push(CallMatch {
                    name,
                    evidence: node_evidence(source_text, node),
                    confidence: 0.98,
                });
            }
        }

        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push(child);
        }
    }

    Some(matches)
}

/// Extract the callable name from a Python AST node that represents the target of a call.
fn python_callable_name(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(source_text, node)),
        // Method calls like `obj.method()` produce an attribute node where
        // the `.name` child holds the method name.
        "attribute" => node
            .child_by_field_name("attribute")
            .and_then(|attr| python_callable_name(source_text, attr))
            .or_else(|| last_python_identifier(source_text, node)),
        // Subscript calls like `arr[0]()` — extract from the subscripted object.
        "subscript" => node
            .child_by_field_name("value")
            .and_then(|val| python_callable_name(source_text, val))
            .or_else(|| last_python_identifier(source_text, node)),
        // Lambda calls are rare but possible: `(lambda x: f())()`.
        "lambda" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(name) = python_callable_name(source_text, child) {
                    return Some(name);
                }
            }
            None
        }
        // Parenthesized expressions: `(f)` or `(obj.method)`.
        "parenthesized_expression" => node
            .named_child(0)
            .and_then(|c| python_callable_name(source_text, c)),
        _ => {
            // For unknown wrapper nodes (e.g., `lambda`, comprehension), recurse
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = python_callable_name(source_text, child) {
                    return Some(name);
                }
            }
            None
        }
    }
}

/// Extract the last identifier from a Python AST subtree.
/// Fallback for complex nodes where we cannot determine the exact callable field.
fn last_python_identifier(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(source_text, node)),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "identifier") {
                    return Some(node_text(source_text, child));
                }
            }
            None
        }
    }
}

// ── TypeScript AST call extraction (Issue #85)──────────────────────────
/// Collects all callable name matches from a TypeScript source via tree-sitter.
fn collect_typescript_call_matches(source_text: &str) -> Option<Vec<CallMatch>> {
    let mut parser = Parser::new();
    // Try TYPESCRIPT first (handles both .ts and .tsx files).
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
        .is_ok()
    {
        if let Some(tree) = parser.parse(source_text, None) {
            return collect_ts_call_matches_impl(source_text, &tree);
        }
    }
    // Fallback to TSX grammar (for JSX-heavy files).
    if parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .is_ok()
    {
        let tree = parser.parse(source_text, None)?;
        return collect_ts_call_matches_impl(source_text, &tree);
    }
    Some(Vec::new())
}

fn collect_ts_call_matches_impl(source_text: &str, tree: &Tree) -> Option<Vec<CallMatch>> {
    let mut matches = Vec::new();
    let mut stack = vec![tree.root_node()];

    while let Some(node) = stack.pop() {
        match node.kind() {
            "call_expression" => {
                // TypeScript `call_expression` has fields: function, arguments.
                // Direct calls: foo()
                // Method calls via member access: obj.method(), Obj.staticMethod()
                let target = node.child_by_field_name("function");
                if let Some(name) = target.and_then(|t| ts_callable_name(source_text, t)) {
                    matches.push(CallMatch {
                        name,
                        evidence: node_evidence(source_text, node),
                        confidence: 0.98,
                    });
                }
            }
            "new_expression" => {
                // Constructor calls: new Foo(1)
                let target = node
                    .child_by_field_name("class")
                    .or_else(|| node.named_child(0));
                if let Some(name) = target.and_then(|t| ts_callable_name(source_text, t)) {
                    matches.push(CallMatch {
                        name,
                        evidence: node_evidence(source_text, node),
                        confidence: 0.95,
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

/// Extract the callable name from a TypeScript AST node that represents the target of a call.
fn ts_callable_name(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" | "qualified_name" => Some(node_text(source_text, node)),
        // Method calls via member access: obj.method()
        "member_access_expression" => node
            .child_by_field_name("property")
            .and_then(|prop| ts_callable_name(source_text, prop))
            .or_else(|| last_ts_identifier(source_text, node)),
        // Computed property calls via brackets: obj['method']()
        "computed_member_access_expression" => {
            let prop = node.child_by_field_name("property")?;
            if matches!(prop.kind(), "identifier" | "string_literal") {
                // For computed access, extract the identifier inside
                last_ts_identifier(source_text, prop)
            } else {
                None
            }
        }
        // Parenthesized: (foo) or (obj.method)
        "parenthesized_expression" => node
            .named_child(0)
            .and_then(|c| ts_callable_name(source_text, c)),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(name) = ts_callable_name(source_text, child) {
                    return Some(name);
                }
            }
            None
        }
    }
}

/// Extract the last identifier from a TypeScript AST subtree.
/// Fallback for complex nodes where we cannot determine the exact callable field.
fn last_ts_identifier(source_text: &str, node: Node<'_>) -> Option<String> {
    match node.kind() {
        "identifier" | "qualified_name" => Some(node_text(source_text, node)),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if matches!(child.kind(), "identifier") {
                    return Some(node_text(source_text, child));
                }
            }
            None
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

    fn build_index_files(files: &[(&str, &str)]) -> SymbolIndex {
        let dir = TempDir::new().unwrap();
        for (name, source) in files {
            std::fs::write(dir.path().join(name), source).unwrap();
        }
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        index
    }

    fn outgoing_ids(index: &SymbolIndex, from_name: &str) -> Vec<(String, EdgeRelation, f32)> {
        let from = index
            .symbols
            .values()
            .find(|symbol| symbol.name == from_name)
            .unwrap();
        index
            .graph
            .outgoing
            .get(&from.id)
            .into_iter()
            .flatten()
            .map(|edge| {
                (
                    index
                        .symbols
                        .get(&edge.symbol_id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default(),
                    edge.relation,
                    edge.confidence,
                )
            })
            .collect()
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

    // ── Issue #76 fixtures ──────────────────────────────────────────────

    #[test]
    fn test_comment_mentioning_call_produces_no_calls_edge() {
        // A Python comment containing `authenticate_user()` previously
        // produced ~0.98-confidence `calls` edges to the Python function
        // and an unrelated same-name Rust function.
        let index = build_index_files(&[
            (
                "svc.py",
                "def authenticate_user():\n    pass\n\ndef caller():\n    # authenticate_user() handles login\n    return None\n",
            ),
            ("auth.rs", "fn authenticate_user() {}\n"),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert!(
            edges.is_empty(),
            "comment-only mention must produce no edges, got {edges:?}"
        );
    }

    #[test]
    fn test_string_mentioning_call_produces_no_calls_edge() {
        let index =
            build_index("fn helper() {}\nfn root() { let msg = \"please call helper() now\"; }\n");

        let edges = outgoing_ids(&index, "root");
        assert!(
            edges.is_empty(),
            "string-only mention must produce no edges, got {edges:?}"
        );
    }

    #[test]
    fn test_python_docstring_mention_does_not_become_call() {
        let index = build_index_files(&[
            (
                "svc.py",
                "def helper():\n    pass\n\ndef documented():\n    \"\"\"\n    Use helper() before closing.\n    \"\"\"\n    return None\n",
            ),
        ]);

        let edges = outgoing_ids(&index, "documented");
        assert!(
            !edges
                .iter()
                .any(|(_, relation, _)| *relation == EdgeRelation::Calls),
            "docstring mention must not become a calls edge, got {edges:?}"
        );
    }

    #[test]
    fn test_cross_language_name_match_is_not_resolved() {
        // A Rust call to `handler` must not create edges to the unrelated
        // same-name Python symbol.
        let index = build_index_files(&[
            ("lib.rs", "fn handler() {}\nfn root() { handler(); }\n"),
            ("mod.py", "def handler():\n    pass\n"),
        ]);

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "handler");
        assert_eq!(edges[0].1, EdgeRelation::Calls);

        // And the referenced target must be the Rust symbol.
        let root = index.symbols.values().find(|s| s.name == "root").unwrap();
        let edge = &index.graph.outgoing.get(&root.id).unwrap()[0];
        let target = index.symbols.get(&edge.symbol_id).unwrap();
        assert_eq!(target.language, Language::Rust);
    }

    #[test]
    fn test_ambiguous_name_match_is_reference_not_call() {
        // Two same-language candidates share the name `process`: the call
        // site cannot distinguish them, so both stay `References`.
        let index = build_index_files(&[
            ("lib.rs", "fn process() {}\nfn root() { process(); }\n"),
            ("other.rs", "fn process() {}\n"),
        ]);

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 2, "got {edges:?}");
        for (_, relation, confidence) in &edges {
            assert_eq!(*relation, EdgeRelation::References);
            assert!(*confidence <= 0.84, "got {confidence}");
        }
    }

    #[test]
    fn test_unambiguous_call_still_classified_as_calls() {
        // Guard: the ambiguity cap must not degrade genuine calls.
        let index = build_index_files(&[
            ("lib.rs", "fn process() {}\nfn root() { process(); }\n"),
            ("mod.py", "def process():\n    pass\n"),
        ]);

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "process");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
    }

    // ── Call-shaped generic evidence (macro-embedded calls) ────────────

    #[test]
    fn test_call_inside_macro_arguments_is_call() {
        // tree-sitter parses macro token trees as raw tokens, so the Rust
        // AST scan cannot see calls embedded in `format!`/`println!`
        // arguments. A direct `name(` occurrence in cleaned source is still
        // a genuine call site.
        let index = build_index(
            "fn helper_fn(s: &str) -> &str { s }\nfn root() {\n    let s = helper_fn(\"x\");\n    println!(\"{}\", helper_fn(s));\n    summary.push_str(&format!(\"{}\", helper_fn(\"y\")));\n}\n",
        );

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "helper_fn");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
        assert!(edges[0].2 >= 0.98, "got {}", edges[0].2);
    }

    #[test]
    fn test_name_mention_without_call_shape_stays_reference() {
        // The identifier appears without a following `(` — the parentheses
        // on the line belong to something else. Must stay a reference.
        let index = build_index(
            "fn helper_fn(s: &str) -> &str { s }\nfn root() {\n    let f: fn(&str) -> &str = helper_fn;\n    println!(\"{} {}\", f, 1 + (2 * 3));\n}\n",
        );

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "helper_fn");
        assert_eq!(edges[0].1, EdgeRelation::References);
        assert!(edges[0].2 <= 0.84, "got {}", edges[0].2);
    }

    #[test]
    fn test_call_shaped_ambiguous_name_stays_reference() {
        // Even with a direct `name(` occurrence, ambiguity keeps the edge
        // below the calls threshold.
        let index = build_index_files(&[
            (
                "lib.rs",
                "fn process() {}\nfn root() { format!(\"{}\", process()); }\n",
            ),
            ("other.rs", "fn process() {}\n"),
        ]);

        let edges = outgoing_ids(&index, "root");
        assert_eq!(edges.len(), 2, "got {edges:?}");
        for (_, relation, confidence) in &edges {
            assert_eq!(*relation, EdgeRelation::References);
            assert!(*confidence <= 0.84, "got {confidence}");
        }
    }

    #[test]
    fn test_method_call_shape_with_dot_prefix_is_call() {
        // `.name(` is a method call site; the dot must not defeat the
        // call-shape detection (preceding `.` is not a word character).
        let index = build_index(
            "struct Worker;\nimpl Worker { fn run(&self) {} }\nfn root(w: &Worker) { println!(\"{}\", w.run()); }\n",
        );

        let run_edge = outgoing_edge(&index, "root", "run").unwrap();
        assert_eq!(run_edge.relation, EdgeRelation::Calls);
    }

    #[test]
    fn test_ast_and_generic_edges_for_same_target_are_deduplicated() {
        // `leaf()` produces an AST call edge (0.99) and a call-shaped generic
        // edge (0.98). The bucket must keep only the higher-priority one per
        // target — confidence-descending sort separates same-target edges,
        // so consecutive-dedup is insufficient.
        let index = build_index("fn leaf() {}\nfn branch() { leaf(); format!(\"{}\", leaf()); }\n");

        let edges = outgoing_ids(&index, "branch");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "leaf");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
        assert!(edges[0].2 >= 0.98, "got {}", edges[0].2);
    }

    #[test]
    fn test_ast_and_generic_edges_for_same_target_are_deduplicated_in_buckets() {
        // Same scenario checked through the NavigationEdge buckets directly,
        // including the incoming direction.
        let index = build_index("fn leaf() {}\nfn branch() { leaf(); format!(\"{}\", leaf()); }\n");
        let leaf = index.symbols.values().find(|s| s.name == "leaf").unwrap();
        let incoming = index.graph.incoming.get(&leaf.id).unwrap();
        assert_eq!(incoming.len(), 1, "got {incoming:?}");
    }

    #[test]
    fn test_contains_call_pattern_boundaries() {
        assert!(contains_call_pattern("let x = foo(1);", "foo"));
        assert!(!contains_call_pattern("let x = my_foo(1);", "foo"));
        assert!(!contains_call_pattern("let x = foo2(1);", "foo"));
        assert!(!contains_call_pattern("let foo = bar(1);", "foo"));
        assert!(contains_call_pattern("w.run();", "run"));
        assert!(!contains_call_pattern("x = foo (1);", "foo"));
        // NOTE: comment/string stripping happens upstream (in
        // scan_generic_direct_references); this helper only sees cleaned
        // text, and the stripped case is covered by
        // test_comment_mentioning_call_produces_no_calls_edge.
    }

    // ── Issue #85: Python AST call extraction tests ───────────────

    #[test]
    fn test_python_ast_direct_call_is_extracted() {
        let matches = collect_python_call_matches(
            r#"def helper():
    pass
def caller():
    helper(1, 2)
"#,
        );
        assert!(matches.is_some(), "parsing should succeed");
        let matches = matches.unwrap();
        // Should find one call: helper()
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "helper");
    }

    #[test]
    fn test_python_ast_method_call_is_extracted() {
        let matches = collect_python_call_matches(
            r#"class Greeter:
    def greet(self):
        pass
def caller():
    g.greet('hello')
"#,
        );
        assert!(matches.is_some());
        let matches = matches.unwrap();
        // Should find one call: greet()
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].name, "greet",
            "method name should be extracted from attribute access"
        );
    }

    #[test]
    fn test_python_ast_nested_calls_are_extracted() {
        let matches = collect_python_call_matches(
            r#"def outer():
def inner():
    pass
def caller():
    result = outer(inner()) + foo(bar(1))
"#,
        );
        assert!(matches.is_some());
        let matches = matches.unwrap();
        // Should find: inner(), outer(), bar(), foo()
        assert_eq!(
            matches.len(),
            4,
            "got {:?}",
            matches.iter().map(|m| m.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_python_graph_direct_call_produces_calls_edge() {
        let index = build_index_files(&[
            (
                r#"helper.py"#,
                r#"def helper():
    pass
"#,
            ),
            (
                r#"caller.py"#,
                r#"def caller():
    result = helper(1, 2)
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "helper");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
    }

    #[test]
    fn test_python_graph_method_call_produces_calls_edge() {
        let index = build_index_files(&[
            (
                r#"class_def.py"#,
                r#"class Greeter:
    def greet(self):
        pass
"#,
            ),
            (
                r#"caller.py"#,
                r#"def caller():
    g.greet('hello')
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "greet");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
    }

    #[test]
    fn test_python_ambiguous_name_stays_reference() {
        let index = build_index_files(&[
            (
                r#"a.py"#,
                r#"def process():
    pass
def caller():
    process(x)
"#,
            ),
            (
                r#"b.py"#,
                r#"def process():
    return True
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 2, "got {edges:?}");
        for (_, relation, confidence) in &edges {
            assert_eq!(
                *relation,
                EdgeRelation::References,
                "ambiguous name should not become Calls edge"
            );
            assert!(
                *confidence <= AMBIGUOUS_CONFIDENCE_CAP,
                "confidence capped at {}",
                *confidence
            );
        }
    }

    // ── Issue #85: TypeScript AST call extraction tests ───────────

    #[test]
    fn test_typescript_ast_direct_call_is_extracted() {
        let matches = collect_typescript_call_matches(
            r#"function helper(): void {}
caller();
"#,
        );
        assert!(matches.is_some(), "parsing should succeed");
        let matches = matches.unwrap();
        // Should find one call: caller()
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "caller");
    }

    #[test]
    fn test_typescript_ast_method_call_is_extracted() {
        let matches = collect_typescript_call_matches(
            r#"class Greeter {
    greet(): void {}
}
caller();
g.greet('hello');
"#,
        );
        assert!(matches.is_some());
        let matches = matches.unwrap();
        // Should find: caller() and g.greet()
        assert_eq!(
            matches.len(),
            2,
            "got {:?}",
            matches.iter().map(|m| m.name.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_typescript_graph_direct_call_produces_calls_edge() {
        let index = build_index_files(&[
            (
                r#"helper.ts"#,
                r#"function helper(): void {}
"#,
            ),
            (
                r#"caller.ts"#,
                r#"function caller(): void {
    result = helper(1, 2);
}
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "helper");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
    }

    #[test]
    fn test_typescript_graph_method_call_produces_calls_edge() {
        let index = build_index_files(&[
            (
                r#"class_def.ts"#,
                r#"class Greeter {
    greet(): void {}
}
"#,
            ),
            (
                r#"caller.ts"#,
                r#"function caller(): void {
    g.greet('hello');
}
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].0, "greet");
        assert_eq!(edges[0].1, EdgeRelation::Calls);
    }

    #[test]
    fn test_typescript_ambiguous_name_stays_reference() {
        let index = build_index_files(&[
            (
                r#"a.ts"#,
                r#"function process(): void {}
function caller(): void {
    process(x);
}
"#,
            ),
            (
                r#"b.ts"#,
                r#"                function process(): number { return 1; }
"#,
            ),
        ]);

        let edges = outgoing_ids(&index, "caller");
        assert_eq!(edges.len(), 2, "got {edges:?}");
        for (_, relation, confidence) in &edges {
            assert_eq!(
                *relation,
                EdgeRelation::References,
                "ambiguous name should not become Calls edge"
            );
            assert!(
                *confidence <= AMBIGUOUS_CONFIDENCE_CAP,
                "confidence capped at {}",
                *confidence
            );
        }
    }
}
