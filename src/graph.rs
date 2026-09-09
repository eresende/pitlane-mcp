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
    pub resolution: EdgeResolution,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeResolution {
    #[default]
    Resolved,
    Ambiguous,
}

impl EdgeResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnresolvedCall {
    pub name: String,
    pub receiver: Option<String>,
    pub evidence: String,
    pub confidence: f32,
    pub reason: String,
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
    pub resolution: EdgeResolution,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationGraph {
    pub built: bool,
    pub outgoing: HashMap<String, Vec<NavigationEdge>>,
    pub incoming: HashMap<String, Vec<NavigationEdge>>,
    pub unresolved_calls: HashMap<String, Vec<UnresolvedCall>>,
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

fn read_file_source(sym: &Symbol) -> anyhow::Result<String> {
    let mut file = open_regular_file(sym.file.as_ref())
        .map_err(|e| anyhow::anyhow!("Cannot open file {:?}: {}", sym.file, e))?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
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
    build_navigation_graph_with_source(index, read_file_source)
}

/// Build the same navigation graph from a revision snapshot without reading the worktree.
pub(crate) fn build_navigation_graph_with_source(
    index: &SymbolIndex,
    file_source: impl Fn(&Symbol) -> anyhow::Result<String>,
) -> NavigationGraph {
    let mut graph = NavigationGraph {
        built: true,
        ..Default::default()
    };

    let candidates = CandidateIndex::build(index);
    let mut sources = HashMap::new();
    let mut failed_sources = HashSet::new();
    let mut imports_by_file = HashMap::new();
    for sym in index.symbols.values() {
        if failed_sources.contains(sym.file.as_ref()) {
            continue;
        }
        if !sources.contains_key(sym.file.as_ref()) {
            let Ok(source) = file_source(sym) else {
                failed_sources.insert(sym.file.as_ref().clone());
                continue;
            };
            sources.insert(sym.file.as_ref().clone(), source);
        }
        let Some(source_text) = sources.get(sym.file.as_ref()) else {
            continue;
        };
        let Some(symbol_source) = source_text.get(sym.byte_start..sym.byte_end) else {
            continue;
        };
        let imports = imports_by_file
            .entry(sym.file.as_ref().clone())
            .or_insert_with(|| parse_imports(&sym.language, source_text));
        let scanned = scan_direct_references(
            index,
            sym,
            symbol_source,
            source_text,
            Some(imports),
            &candidates,
        );
        if !scanned.unresolved.is_empty() {
            graph
                .unresolved_calls
                .insert(sym.id.clone(), scanned.unresolved);
        }
        for reference in scanned.references {
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
                    resolution: reference.resolution,
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
                    resolution: reference.resolution,
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
    let owned_file_source = read_file_source(sym).ok();
    let file_source = owned_file_source.as_deref().unwrap_or(source_text);
    scan_direct_references(
        index,
        sym,
        source_text,
        file_source,
        None,
        &CandidateIndex::build(index),
    )
    .references
}

pub fn collect_unresolved_calls(index: &SymbolIndex, sym: &Symbol) -> Vec<UnresolvedCall> {
    if index.graph.built {
        return index
            .graph
            .unresolved_calls
            .get(&sym.id)
            .cloned()
            .unwrap_or_default();
    }
    let Ok(file_source) = read_file_source(sym) else {
        return Vec::new();
    };
    let Some(symbol_source) = file_source.get(sym.byte_start..sym.byte_end) else {
        return Vec::new();
    };
    scan_direct_references(
        index,
        sym,
        symbol_source,
        &file_source,
        None,
        &CandidateIndex::build(index),
    )
    .unresolved
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
                resolution: reference.resolution,
            });
        }
    }
    sort_direct_references(&mut callers);
    callers
}

#[derive(Default)]
struct ScanResult {
    references: Vec<DirectReference>,
    unresolved: Vec<UnresolvedCall>,
}

fn scan_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
    file_source: &str,
    file_imports: Option<&[ImportBinding]>,
    candidates: &CandidateIndex,
) -> ScanResult {
    let mut refs = Vec::new();
    let mut unresolved = Vec::new();
    let mut ast_call_names = HashSet::new();
    let cap_generic_confidence = match sym.language {
        Language::Rust => match scan_rust_direct_references(index, sym, source_text, candidates) {
            Some(mut rust_refs) => {
                refs.append(&mut rust_refs);
                Some(0.84)
            }
            None => None,
        },
        Language::Python | Language::TypeScript => {
            let extraction = match sym.language {
                Language::Python => collect_python_call_matches(source_text),
                Language::TypeScript => collect_typescript_call_matches(source_text, &sym.file),
                _ => unreachable!(),
            };
            if let Some(extraction) = extraction {
                let context = ResolutionContext::from_source(
                    &sym.language,
                    file_source,
                    source_text,
                    file_imports,
                );
                for matched in extraction {
                    ast_call_names.insert(matched.name.clone());
                    match resolve_ast_match(index, sym, &matched, candidates, &context) {
                        AstResolution::References(mut resolved) => refs.append(&mut resolved),
                        AstResolution::Unresolved(call) => unresolved.push(call),
                    }
                }
            }
            Some(0.84)
        }
        _ => None,
    };

    let mut generic =
        scan_generic_direct_references(index, sym, source_text, cap_generic_confidence, candidates);
    generic.retain(|reference| !ast_call_names.contains(&reference.name));
    refs.extend(generic);
    sort_direct_references(&mut refs);
    unresolved.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.evidence.cmp(&b.evidence))
            .then_with(|| a.reason.cmp(&b.reason))
    });
    unresolved
        .dedup_by(|a, b| a.name == b.name && a.evidence == b.evidence && a.reason == b.reason);
    ScanResult {
        references: refs,
        unresolved,
    }
}

/// Confidence cap for references whose target name is ambiguous (multiple
/// same-language candidates). Below the `Calls` thresholds in
/// `classify_relation`, so ambiguous name matches stay `References`.
const AMBIGUOUS_CONFIDENCE_CAP: f32 = 0.84;

#[derive(Debug, Clone)]
struct CallMatch {
    name: String,
    receiver: Option<String>,
    evidence: String,
    confidence: f32,
}

#[derive(Debug, Clone)]
struct ImportBinding {
    local: String,
    imported: Option<String>,
    module: String,
    namespace: bool,
}

#[derive(Debug, Default)]
struct ResolutionContext {
    imports: Vec<ImportBinding>,
    shadowed: HashSet<String>,
    receiver_types: HashMap<String, String>,
}

impl ResolutionContext {
    fn from_source(
        language: &Language,
        file_source: &str,
        symbol_source: &str,
        cached_imports: Option<&[ImportBinding]>,
    ) -> Self {
        let imports = cached_imports
            .map(<[ImportBinding]>::to_vec)
            .unwrap_or_else(|| parse_imports(language, file_source));
        let (shadowed, receiver_types) = collect_local_bindings(language, symbol_source);
        Self {
            imports,
            shadowed,
            receiver_types,
        }
    }
}

fn parse_imports(language: &Language, source: &str) -> Vec<ImportBinding> {
    match language {
        Language::Python => parse_python_imports(source),
        Language::TypeScript => parse_typescript_imports(source),
        _ => Vec::new(),
    }
}

enum AstResolution {
    References(Vec<DirectReference>),
    Unresolved(UnresolvedCall),
}

fn resolve_ast_match(
    index: &SymbolIndex,
    caller: &Symbol,
    matched: &CallMatch,
    candidates: &CandidateIndex,
    context: &ResolutionContext,
) -> AstResolution {
    let unresolved = |reason: &str| {
        AstResolution::Unresolved(UnresolvedCall {
            name: matched.name.clone(),
            receiver: matched.receiver.clone(),
            evidence: matched.evidence.clone(),
            confidence: matched.confidence,
            reason: reason.to_string(),
        })
    };

    let resolved = if let Some(receiver) = matched.receiver.as_deref() {
        if let Some(binding) = context
            .imports
            .iter()
            .find(|binding| binding.namespace && binding.local == receiver)
        {
            filter_module_candidates(
                candidates.lookup(index, caller.language.clone(), &matched.name, &caller.id),
                caller,
                &binding.module,
            )
        } else {
            let owner = receiver_owner(caller, receiver, context, candidates, index);
            let Some((owner, module)) = owner else {
                return unresolved("dynamic_receiver");
            };
            let methods = candidates
                .lookup(index, caller.language.clone(), &matched.name, &caller.id)
                .into_iter()
                .filter(|candidate| qualified_owner_matches(candidate, &owner))
                .collect::<Vec<_>>();
            if let Some(module) = module {
                filter_module_candidates(methods, caller, &module)
            } else {
                methods
            }
        }
    } else {
        if context.shadowed.contains(&matched.name) {
            return unresolved("shadowed_local_binding");
        }

        let same_file = candidates
            .lookup(index, caller.language.clone(), &matched.name, &caller.id)
            .into_iter()
            .filter(|candidate| candidate.file == caller.file)
            .collect::<Vec<_>>();
        let lexical = prefer_lexical_candidates(caller, same_file);
        if !lexical.is_empty() {
            lexical
        } else if let Some(binding) = context
            .imports
            .iter()
            .find(|binding| !binding.namespace && binding.local == matched.name)
        {
            binding_candidates(index, caller, candidates, binding, &matched.name)
        } else {
            Vec::new()
        }
    };

    if resolved.is_empty() {
        return unresolved("no_visible_target");
    }
    AstResolution::References(refs_from_candidates(resolved, matched))
}

fn binding_candidates<'a>(
    index: &'a SymbolIndex,
    caller: &Symbol,
    candidates: &CandidateIndex,
    binding: &ImportBinding,
    fallback_name: &str,
) -> Vec<&'a Symbol> {
    if binding.imported.as_deref() == Some("default") {
        return index
            .symbols
            .values()
            .filter(|candidate| {
                candidate.id != caller.id
                    && candidate.language == caller.language
                    && is_callable_kind(&candidate.kind)
                    && module_matches(
                        &candidate.file,
                        &caller.file,
                        &binding.module,
                        &caller.language,
                    )
            })
            .collect();
    }
    let target_name = binding.imported.as_deref().unwrap_or(fallback_name);
    filter_module_candidates(
        candidates.lookup(index, caller.language.clone(), target_name, &caller.id),
        caller,
        &binding.module,
    )
}

fn refs_from_candidates(resolved: Vec<&Symbol>, matched: &CallMatch) -> Vec<DirectReference> {
    let resolution = if resolved.len() > 1 {
        EdgeResolution::Ambiguous
    } else {
        EdgeResolution::Resolved
    };
    resolved
        .into_iter()
        .map(|candidate| DirectReference {
            id: candidate.id.clone(),
            name: candidate.name.clone(),
            kind: candidate.kind.to_string(),
            file: candidate.file.to_string_lossy().replace('\\', "/"),
            line_start: candidate.line_start,
            evidence: matched.evidence.chars().take(240).collect(),
            confidence: if resolution == EdgeResolution::Ambiguous {
                matched.confidence.min(AMBIGUOUS_CONFIDENCE_CAP)
            } else {
                matched.confidence
            },
            resolution,
        })
        .collect()
}

fn prefer_lexical_candidates<'a>(caller: &Symbol, candidates: Vec<&'a Symbol>) -> Vec<&'a Symbol> {
    let separator = match caller.language {
        Language::Python => "::",
        Language::TypeScript => ".",
        _ => return candidates,
    };
    let owner = caller
        .qualified
        .rsplit_once(separator)
        .map(|(owner, _)| owner);
    if let Some(owner) = owner {
        let scoped: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| {
                candidate
                    .qualified
                    .strip_suffix(&format!("{separator}{}", candidate.name))
                    == Some(owner)
            })
            .collect();
        if !scoped.is_empty() {
            return scoped;
        }
    }
    candidates
        .into_iter()
        .filter(|candidate| !candidate.qualified.contains(separator))
        .collect()
}

fn qualified_owner_matches(candidate: &Symbol, owner: &str) -> bool {
    candidate.qualified == format!("{owner}::{}", candidate.name)
        || candidate.qualified == format!("{owner}.{}", candidate.name)
}

fn receiver_owner(
    caller: &Symbol,
    receiver: &str,
    context: &ResolutionContext,
    candidates: &CandidateIndex,
    index: &SymbolIndex,
) -> Option<(String, Option<String>)> {
    if matches!(receiver, "self" | "cls" | "this") {
        return caller
            .qualified
            .rsplit_once(if caller.language == Language::Python {
                "::"
            } else {
                "."
            })
            .map(|(owner, _)| (owner.to_string(), None));
    }
    let inferred_owner = context
        .receiver_types
        .get(receiver)
        .map(String::as_str)
        .unwrap_or(receiver);
    if let Some(binding) = context
        .imports
        .iter()
        .find(|binding| !binding.namespace && binding.local == inferred_owner)
    {
        if binding.imported.as_deref() == Some("default") {
            let classes: Vec<_> = index
                .symbols
                .values()
                .filter(|candidate| {
                    candidate.language == caller.language
                        && candidate.kind == SymbolKind::Class
                        && module_matches(
                            &candidate.file,
                            &caller.file,
                            &binding.module,
                            &caller.language,
                        )
                })
                .collect();
            if classes.len() == 1 {
                return Some((classes[0].name.clone(), Some(binding.module.clone())));
            }
            return None;
        }
        return binding
            .imported
            .clone()
            .map(|owner| (owner, Some(binding.module.clone())));
    }
    let class_matches =
        candidates.lookup(index, caller.language.clone(), inferred_owner, &caller.id);
    let same_file: Vec<_> = class_matches
        .iter()
        .copied()
        .filter(|candidate| candidate.file == caller.file)
        .collect();
    if same_file.len() == 1 && same_file[0].kind == SymbolKind::Class {
        return Some((same_file[0].name.clone(), None));
    }
    (class_matches.len() == 1 && class_matches[0].kind == SymbolKind::Class)
        .then(|| (class_matches[0].name.clone(), None))
}

fn filter_module_candidates<'a>(
    candidates: Vec<&'a Symbol>,
    caller: &Symbol,
    module: &str,
) -> Vec<&'a Symbol> {
    candidates
        .into_iter()
        .filter(|candidate| module_matches(&candidate.file, &caller.file, module, &caller.language))
        .collect()
}

fn module_matches(
    candidate: &std::path::Path,
    caller: &std::path::Path,
    module: &str,
    language: &Language,
) -> bool {
    let candidate = candidate.to_string_lossy().replace('\\', "/");
    let caller_dir = caller.parent().unwrap_or_else(|| std::path::Path::new(""));
    let module = module.trim_matches(|c| matches!(c, '\'' | '"'));
    let relative = module.starts_with('.') && !matches!(language, Language::Python)
        || module.starts_with("./")
        || module.starts_with("../");
    let module_path = if *language == Language::Python {
        let leading = module.chars().take_while(|c| *c == '.').count();
        let mut base = caller_dir.to_path_buf();
        for _ in 1..leading {
            base.pop();
        }
        let rest = module.trim_start_matches('.').replace('.', "/");
        if leading > 0 {
            base.join(rest).to_string_lossy().replace('\\', "/")
        } else {
            rest
        }
    } else if relative {
        normalize_path(&caller_dir.join(module))
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        module.to_string()
    };
    let stem = candidate
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&candidate);
    stem == module_path
        || stem.ends_with(&format!("/{module_path}"))
        || stem == format!("{module_path}/index")
        || stem.ends_with(&format!("/{module_path}/index"))
        || stem == format!("{module_path}/__init__")
        || stem.ends_with(&format!("/{module_path}/__init__"))
}

fn normalize_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn parse_python_imports(source: &str) -> Vec<ImportBinding> {
    let mut bindings = Vec::new();
    for statement in import_statements(source, &["from ", "import "]) {
        let line = statement.split('#').next().unwrap_or("").trim();
        if let Some(rest) = line.strip_prefix("from ") {
            let Some((module, names)) = rest.split_once(" import ") else {
                continue;
            };
            for name in names.trim_matches(|c| matches!(c, '(' | ')')).split(',') {
                let (imported, local) = split_alias(name.trim());
                if !imported.is_empty() {
                    bindings.push(ImportBinding {
                        local: local.to_string(),
                        imported: Some(imported.to_string()),
                        module: module.trim().to_string(),
                        namespace: false,
                    });
                }
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            for item in rest.split(',') {
                let (module, alias) = split_alias(item.trim());
                if !module.is_empty() {
                    bindings.push(ImportBinding {
                        local: if alias == module {
                            module.split('.').next().unwrap_or(module).to_string()
                        } else {
                            alias.to_string()
                        },
                        imported: None,
                        module: module.to_string(),
                        namespace: true,
                    });
                }
            }
        }
    }
    bindings
}

fn parse_typescript_imports(source: &str) -> Vec<ImportBinding> {
    let mut bindings = Vec::new();
    for statement in import_statements(source, &["import "]) {
        let line = statement.trim();
        let Some(rest) = line.strip_prefix("import ") else {
            continue;
        };
        let Some((clause, module)) = rest.rsplit_once(" from ") else {
            continue;
        };
        let module = module
            .split(';')
            .next()
            .unwrap_or(module)
            .trim()
            .trim_end_matches(';')
            .trim_matches(|c| matches!(c, '\'' | '"'));
        if let Some(namespace) = clause.trim().strip_prefix("* as ") {
            bindings.push(ImportBinding {
                local: namespace.trim().to_string(),
                imported: None,
                module: module.to_string(),
                namespace: true,
            });
            continue;
        }
        if let (Some(start), Some(end)) = (clause.find('{'), clause.rfind('}')) {
            for name in clause[start + 1..end].split(',') {
                let (imported, local) = split_alias(name.trim());
                if !imported.is_empty() {
                    bindings.push(ImportBinding {
                        local: local.to_string(),
                        imported: Some(imported.to_string()),
                        module: module.to_string(),
                        namespace: false,
                    });
                }
            }
        }
        let default = clause.split(',').next().unwrap_or("").trim();
        if !default.is_empty() && !default.starts_with('{') {
            bindings.push(ImportBinding {
                local: default.to_string(),
                imported: Some("default".to_string()),
                module: module.to_string(),
                namespace: false,
            });
        }
    }
    bindings
}

fn import_statements(source: &str, prefixes: &[&str]) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut nesting = 0_i32;
    for line in source.lines().map(str::trim) {
        if current.is_empty() {
            if !prefixes.iter().any(|prefix| line.starts_with(prefix)) {
                continue;
            }
        } else {
            current.push(' ');
        }
        current.push_str(line.trim_end_matches('\\'));
        nesting += line
            .chars()
            .map(|c| match c {
                '(' | '{' | '[' => 1,
                ')' | '}' | ']' => -1,
                _ => 0,
            })
            .sum::<i32>();
        if nesting <= 0 && !line.ends_with('\\') {
            statements.push(std::mem::take(&mut current));
            nesting = 0;
        }
    }
    if !current.is_empty() {
        statements.push(current);
    }
    statements
}

fn split_alias(value: &str) -> (&str, &str) {
    value
        .split_once(" as ")
        .map(|(original, alias)| (original.trim(), alias.trim()))
        .unwrap_or((value.trim(), value.trim()))
}

fn collect_local_bindings(
    language: &Language,
    source: &str,
) -> (HashSet<String>, HashMap<String, String>) {
    let mut shadowed = HashSet::new();
    let mut receiver_types = HashMap::new();
    for line in source.lines().map(str::trim) {
        let declaration = match language {
            Language::TypeScript => ["const ", "let ", "var "]
                .into_iter()
                .find_map(|prefix| line.strip_prefix(prefix)),
            Language::Python => Some(line),
            _ => None,
        };
        if let Some(declaration) = declaration {
            if let Some((left, right)) = declaration.split_once('=') {
                let local = left.trim().split(':').next().unwrap_or("").trim();
                if is_simple_reference(local) {
                    shadowed.insert(local.to_string());
                    let annotated_owner = left.split_once(':').and_then(|(_, owner)| {
                        owner
                            .trim()
                            .split(|c: char| !c.is_alphanumeric() && c != '_')
                            .next()
                            .filter(|owner| is_identifier(owner))
                    });
                    let constructed_owner = || {
                        let right = right.trim().strip_prefix("new ").unwrap_or(right.trim());
                        right
                            .split('(')
                            .next()
                            .map(str::trim)
                            .filter(|name| is_identifier(name))
                    };
                    if let Some(owner) = annotated_owner.or_else(constructed_owner) {
                        receiver_types.insert(local.to_string(), owner.to_string());
                    }
                }
            } else if *language == Language::Python {
                if let Some((local, owner)) = declaration.split_once(':') {
                    let local = local.trim();
                    let owner = owner
                        .trim()
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or("");
                    if is_simple_reference(local) && is_identifier(owner) {
                        shadowed.insert(local.to_string());
                        receiver_types.insert(local.to_string(), owner.to_string());
                    }
                }
            }
            if *language == Language::TypeScript {
                let left = declaration.split('=').next().unwrap_or(declaration);
                if let Some((local, owner)) = left.split_once(':') {
                    let local = local.trim();
                    let owner = owner
                        .trim()
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or("");
                    if is_identifier(local) && is_identifier(owner) {
                        shadowed.insert(local.to_string());
                        receiver_types.insert(local.to_string(), owner.to_string());
                    }
                }
            }
        }
    }
    if let Some((_, params)) = source.lines().next().and_then(|line| line.split_once('(')) {
        if let Some((params, _)) = params.split_once(')') {
            for parameter in params.split(',') {
                let parameter = parameter.trim();
                let name = parameter.split(':').next().unwrap_or(parameter).trim();
                if is_identifier(name) {
                    shadowed.insert(name.to_string());
                }
                if let Some((_, owner)) = parameter.split_once(':') {
                    let owner = owner
                        .trim()
                        .split(|c: char| !c.is_alphanumeric() && c != '_')
                        .next()
                        .unwrap_or("");
                    if is_identifier(name) && is_identifier(owner) {
                        receiver_types.insert(name.to_string(), owner.to_string());
                    }
                }
            }
        }
    }
    (shadowed, receiver_types)
}

fn is_simple_reference(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_identifier)
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

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
                resolution: if ambiguous {
                    EdgeResolution::Ambiguous
                } else {
                    EdgeResolution::Resolved
                },
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
                resolution: edge.resolution,
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
            .then_with(|| resolution_rank(b.resolution).cmp(&resolution_rank(a.resolution)))
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

fn resolution_rank(resolution: EdgeResolution) -> u8 {
    match resolution {
        EdgeResolution::Resolved => 2,
        EdgeResolution::Ambiguous => 1,
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
        resolution_rank(b.resolution)
            .cmp(&resolution_rank(a.resolution))
            .then_with(|| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
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
                resolution: if ambiguous {
                    EdgeResolution::Ambiguous
                } else {
                    EdgeResolution::Resolved
                },
            }
        }));
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

fn collect_python_call_matches(source_text: &str) -> Option<Vec<CallMatch>> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source_text, None)?;
    let mut matches = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if node.kind() == "call" {
            if let Some(target) = node.child_by_field_name("function") {
                let (name, receiver, confidence) = python_call_target(source_text, target);
                if !name.is_empty() {
                    matches.push(CallMatch {
                        name,
                        receiver,
                        evidence: node_evidence(source_text, node),
                        confidence,
                    });
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Some(matches)
}

fn python_call_target(source: &str, node: Node<'_>) -> (String, Option<String>, f32) {
    match node.kind() {
        "identifier" => (node_text(source, node), None, 0.98),
        "attribute" => {
            let name = node
                .child_by_field_name("attribute")
                .map(|child| node_text(source, child))
                .unwrap_or_default();
            let receiver = node
                .child_by_field_name("object")
                .map(|child| node_text(source, child));
            (name, receiver, 0.98)
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|child| python_call_target(source, child))
            .unwrap_or_default(),
        _ => (
            node_text(source, node).chars().take(120).collect(),
            Some("<dynamic>".to_string()),
            0.65,
        ),
    }
}

fn collect_typescript_call_matches(
    source_text: &str,
    file: &std::path::Path,
) -> Option<Vec<CallMatch>> {
    let mut parser = Parser::new();
    let tsx = matches!(
        file.extension().and_then(|extension| extension.to_str()),
        Some("tsx" | "jsx")
    );
    let language = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    };
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source_text, None)?;
    collect_ts_call_matches_impl(source_text, &tree)
}

fn collect_ts_call_matches_impl(source_text: &str, tree: &Tree) -> Option<Vec<CallMatch>> {
    let mut matches = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let target = match node.kind() {
            "call_expression" => node.child_by_field_name("function"),
            "new_expression" => node
                .child_by_field_name("constructor")
                .or_else(|| node.child_by_field_name("class"))
                .or_else(|| node.named_child(0)),
            _ => None,
        };
        if let Some(target) = target {
            let (name, receiver, confidence) = ts_call_target(source_text, target);
            if !name.is_empty() {
                matches.push(CallMatch {
                    name,
                    receiver,
                    evidence: node_evidence(source_text, node),
                    confidence: if node.kind() == "new_expression" {
                        confidence.min(0.95)
                    } else {
                        confidence
                    },
                });
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    Some(matches)
}

fn ts_call_target(source: &str, node: Node<'_>) -> (String, Option<String>, f32) {
    match node.kind() {
        "identifier" => (node_text(source, node), None, 0.98),
        "member_expression" | "member_access_expression" => {
            let name = node
                .child_by_field_name("property")
                .map(|child| node_text(source, child))
                .unwrap_or_default();
            let receiver = node
                .child_by_field_name("object")
                .map(|child| node_text(source, child));
            (name, receiver, 0.98)
        }
        "subscript_expression" | "computed_member_access_expression" => {
            let property = node
                .child_by_field_name("index")
                .or_else(|| node.child_by_field_name("property"));
            let receiver = node
                .child_by_field_name("object")
                .map(|child| node_text(source, child));
            if let Some(property) = property {
                let raw = node_text(source, property);
                if matches!(property.kind(), "string" | "string_literal") {
                    return (
                        raw.trim_matches(|c| matches!(c, '\'' | '"')).to_string(),
                        receiver,
                        0.9,
                    );
                }
            }
            (
                node_text(source, node).chars().take(120).collect(),
                Some("<dynamic>".to_string()),
                0.65,
            )
        }
        "parenthesized_expression" => node
            .named_child(0)
            .map(|child| ts_call_target(source, child))
            .unwrap_or_default(),
        _ => (
            node_text(source, node).chars().take(120).collect(),
            Some("<dynamic>".to_string()),
            0.65,
        ),
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
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, source).unwrap();
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

    #[test]
    fn python_import_alias_resolves_to_the_imported_module() {
        let index = build_index_files(&[
            ("a.py", "def process():\n    pass\n"),
            ("b.py", "def process():\n    pass\n"),
            (
                "caller.py",
                "from a import process as run_process\n\ndef caller():\n    run_process()\n",
            ),
        ]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert!(target.file.ends_with("a.py"));
        assert_eq!(edges[0].relation, EdgeRelation::Calls);
        assert_eq!(edges[0].resolution, EdgeResolution::Resolved);
        assert!(edges[0].evidence.contains("run_process()"));
    }

    #[test]
    fn typescript_namespace_import_resolves_member_without_global_name_matching() {
        let index = build_index_files(&[
            ("a.ts", "export function process(): void {}\n"),
            ("b.ts", "export function process(): void {}\n"),
            (
                "caller.ts",
                "import * as utils from './a';\nexport function caller(): void { utils.process(); }\n",
            ),
        ]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert!(target.file.ends_with("a.ts"));
        assert_eq!(edges[0].relation, EdgeRelation::Calls);
    }

    #[test]
    fn typescript_named_alias_resolves_to_the_imported_module() {
        let index = build_index_files(&[
            ("a.ts", "export function process(): void {}\n"),
            ("b.ts", "export function process(): void {}\n"),
            (
                "caller.ts",
                "import { process as runProcess } from './a';\nexport function caller(): void { runProcess(); }\n",
            ),
        ]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert!(target.file.ends_with("a.ts"));
        assert_eq!(edges[0].resolution, EdgeResolution::Resolved);
    }

    #[test]
    fn imported_targets_that_remain_ambiguous_are_explicitly_labeled() {
        let index = build_index_files(&[
            ("a.ts", "export function process(): void {}\n"),
            ("a.tsx", "export function process(): void {}\n"),
            (
                "caller.ts",
                "import { process } from './a';\nexport function caller(): void { process(); }\n",
            ),
        ]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 2, "got {edges:?}");
        assert!(edges.iter().all(|edge| {
            edge.relation == EdgeRelation::References
                && edge.resolution == EdgeResolution::Ambiguous
                && edge.confidence <= AMBIGUOUS_CONFIDENCE_CAP
        }));
    }

    #[test]
    fn lexical_same_file_candidate_wins_over_same_name_in_other_module() {
        let index = build_index_files(&[
            (
                "local.py",
                "def process():\n    pass\n\ndef caller():\n    process()\n",
            ),
            ("other.py", "def process():\n    pass\n"),
        ]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert!(target.file.ends_with("local.py"));
        assert_eq!(edges[0].resolution, EdgeResolution::Resolved);
    }

    #[test]
    fn typed_receiver_resolves_the_matching_python_method() {
        let index = build_index_files(&[(
            "models.py",
            "class A:\n    def save(self):\n        pass\n\nclass B:\n    def save(self):\n        pass\n\ndef caller(item: A):\n    item.save()\n",
        )]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert_eq!(target.qualified, "A::save");
        assert_eq!(edges[0].resolution, EdgeResolution::Resolved);
    }

    #[test]
    fn typed_receiver_resolves_the_matching_typescript_method() {
        let index = build_index_files(&[(
            "models.ts",
            "class A { save(): void {} }\nclass B { save(): void {} }\nexport function caller(item: A): void { item.save(); }\n",
        )]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let edges = index.graph.outgoing.get(&caller.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        let target = index.symbols.get(&edges[0].symbol_id).unwrap();
        assert_eq!(target.qualified, "A.save");
        assert_eq!(edges[0].resolution, EdgeResolution::Resolved);
    }

    #[test]
    fn dynamic_receiver_is_explicitly_unresolved() {
        let index = build_index_files(&[(
            "models.py",
            "class A:\n    def save(self):\n        pass\n\ndef caller(item):\n    item.save()\n",
        )]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        assert!(!index.graph.outgoing.contains_key(&caller.id));
        let unresolved = index.graph.unresolved_calls.get(&caller.id).unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].name, "save");
        assert_eq!(unresolved[0].receiver.as_deref(), Some("item"));
        assert_eq!(unresolved[0].reason, "dynamic_receiver");
    }

    #[test]
    fn missing_direct_target_is_explicitly_unresolved() {
        let index = build_index_files(&[(
            "caller.ts",
            "export function caller(): void { missingTarget(); }\n",
        )]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        let unresolved = index.graph.unresolved_calls.get(&caller.id).unwrap();
        assert_eq!(unresolved[0].name, "missingTarget");
        assert_eq!(unresolved[0].reason, "no_visible_target");
    }

    #[test]
    fn parameter_shadowing_prevents_false_call_edge() {
        let index = build_index_files(&[(
            "local.py",
            "def process():\n    pass\n\ndef caller(process):\n    process()\n",
        )]);
        let caller = index.symbols.values().find(|s| s.name == "caller").unwrap();
        assert!(!index.graph.outgoing.contains_key(&caller.id));
        assert_eq!(
            index.graph.unresolved_calls[&caller.id][0].reason,
            "shadowed_local_binding"
        );
    }

    #[test]
    fn tsx_uses_tsx_grammar_and_resolves_imported_call() {
        let index = build_index_files(&[
            ("helper.ts", "export function helper(): void {}\n"),
            (
                "component.tsx",
                "import { helper } from './helper';\nexport function Component() { return <button onClick={() => helper()}>Go</button>; }\n",
            ),
        ]);
        let component = index
            .symbols
            .values()
            .find(|s| s.name == "Component")
            .unwrap();
        let edges = index.graph.outgoing.get(&component.id).unwrap();
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(
            index.symbols.get(&edges[0].symbol_id).unwrap().name,
            "helper"
        );
    }

    #[test]
    fn rust_ambiguity_is_explicitly_labeled() {
        let index = build_index_files(&[
            ("lib.rs", "fn process() {}\nfn root() { process(); }\n"),
            ("other.rs", "fn process() {}\n"),
        ]);
        let root = index.symbols.values().find(|s| s.name == "root").unwrap();
        let edges = index.graph.outgoing.get(&root.id).unwrap();
        assert!(edges
            .iter()
            .all(|edge| edge.resolution == EdgeResolution::Ambiguous));
    }
}
