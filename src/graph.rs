use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use crate::index::SymbolIndex;
use crate::indexer::language::{Symbol, SymbolKind};

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

pub fn collect_direct_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
) -> Vec<DirectReference> {
    let identifiers = extract_identifiers(source_text);
    let lines: Vec<&str> = source_text.lines().collect();
    let mut refs: Vec<DirectReference> = index
        .symbols
        .values()
        .filter(|candidate| candidate.id != sym.id && identifiers.contains(candidate.name.as_str()))
        .map(|candidate| {
            let (evidence, confidence) =
                reference_evidence(&lines, candidate.name.as_str(), &candidate.kind);
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
        .collect();
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
    refs
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

pub fn collect_direct_callable_references(
    index: &SymbolIndex,
    sym: &Symbol,
    source_text: &str,
) -> Vec<DirectReference> {
    collect_direct_references(index, sym, source_text)
        .into_iter()
        .filter(|reference| {
            let Some(target) = index.symbols.get(&reference.id) else {
                return false;
            };
            is_callable_kind(&target.kind) && !is_low_signal_name(&target.name)
        })
        .collect()
}
