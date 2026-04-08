use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use crate::index::SymbolIndex;
use crate::indexer::language::Symbol;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectReference {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line_start: u32,
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
    let mut refs: Vec<DirectReference> = index
        .symbols
        .values()
        .filter(|candidate| candidate.id != sym.id && identifiers.contains(candidate.name.as_str()))
        .map(|candidate| DirectReference {
            id: candidate.id.clone(),
            name: candidate.name.clone(),
            kind: candidate.kind.to_string(),
            file: candidate.file.to_string_lossy().replace('\\', "/"),
            line_start: candidate.line_start,
        })
        .collect();
    refs.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
            .then_with(|| a.id.cmp(&b.id))
    });
    refs.dedup_by(|a, b| a.id == b.id);
    refs
}
