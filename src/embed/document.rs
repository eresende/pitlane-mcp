use std::collections::HashSet;
use std::path::Path;

use crate::graph::read_symbol_source;
use crate::indexer::language::Symbol;

/// Bumped to 3 when per-symbol document hashes were added to the embed store
/// (issue #75): stores written by older versions have no hashes, so forcing a
/// full rebuild avoids decoding problems and stale vectors.
pub const DOCUMENT_FORMAT_VERSION: u32 = 3;
const DEFAULT_MAX_CHARS: usize = 6000;
const DEFAULT_BODY_CHARS: usize = 3000;
const DEFAULT_IDENTIFIERS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentProfile {
    Legacy,
    Metadata,
    MetadataCode,
}

impl DocumentProfile {
    pub fn from_env() -> Self {
        match std::env::var("PITLANE_EMBED_DOCUMENT_PROFILE")
            .unwrap_or_else(|_| "metadata_code".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "legacy" => Self::Legacy,
            "metadata" => Self::Metadata,
            _ => Self::MetadataCode,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Metadata => "metadata",
            Self::MetadataCode => "metadata_code",
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .take_while(|(index, _)| *index < max)
        .last()
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    text[..end].to_string()
}

fn compact_code(source: &str, max: usize) -> String {
    if source.len() <= max {
        return source.to_string();
    }
    let head = max * 2 / 3;
    let tail = max - head;
    format!(
        "{}\n/* ... middle omitted ... */\n{}",
        truncate_chars(source, head),
        source
            .char_indices()
            .rev()
            .take_while(|(index, _)| source.len() - *index <= tail)
            .last()
            .map(|(index, _)| &source[index..])
            .unwrap_or("")
    )
}

fn identifiers(source: &str, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    source
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .filter(|item| item.len() >= 3 && !item.chars().all(|ch| ch.is_ascii_digit()))
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .take(limit)
        .map(ToOwned::to_owned)
        .collect()
}

/// 64-bit blake3 hash of an embedding document. Stored per symbol so the
/// embedding pipeline can detect that a symbol's document changed even when
/// the symbol ID stayed the same.
pub fn document_hash(text: &str) -> u64 {
    let bytes = blake3::hash(text.as_bytes());
    u64::from_le_bytes(bytes.as_bytes()[..8].try_into().expect("8 bytes"))
}

pub fn document_fingerprint(model: &str) -> String {
    let profile = DocumentProfile::from_env();
    format!(
        "v{};model={};profile={};max={};body={};identifiers={};document_prefix={};query_prefix={}",
        DOCUMENT_FORMAT_VERSION,
        model,
        profile.label(),
        env_usize("PITLANE_EMBED_MAX_CHARS", DEFAULT_MAX_CHARS),
        env_usize("PITLANE_EMBED_BODY_CHARS", DEFAULT_BODY_CHARS),
        env_usize("PITLANE_EMBED_MAX_IDENTIFIERS", DEFAULT_IDENTIFIERS),
        document_prefix(model),
        query_prefix(model),
    )
}

fn configured_prefix(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
}

pub fn document_prefix(model: &str) -> String {
    if let Some(prefix) = configured_prefix("PITLANE_EMBED_DOCUMENT_PREFIX") {
        return prefix;
    }
    let mode =
        std::env::var("PITLANE_EMBED_TASK_PREFIX_MODE").unwrap_or_else(|_| "auto".to_string());
    if mode == "nomic" || (mode == "auto" && model.to_ascii_lowercase().contains("nomic")) {
        "search_document: ".to_string()
    } else {
        String::new()
    }
}

pub fn query_prefix(model: &str) -> String {
    if let Some(prefix) = configured_prefix("PITLANE_EMBED_QUERY_PREFIX") {
        return prefix;
    }
    let mode =
        std::env::var("PITLANE_EMBED_TASK_PREFIX_MODE").unwrap_or_else(|_| "auto".to_string());
    if mode == "nomic" || (mode == "auto" && model.to_ascii_lowercase().contains("nomic")) {
        "search_query: ".to_string()
    } else {
        String::new()
    }
}

pub fn build_symbol_document(sym: &Symbol, project_root: Option<&Path>) -> String {
    let profile = DocumentProfile::from_env();
    if profile == DocumentProfile::Legacy {
        let mut parts = vec![sym.name.clone(), sym.qualified.clone()];
        parts.extend(sym.signature.iter().cloned());
        parts.extend(sym.doc.iter().cloned());
        return truncate_chars(
            &parts.join("\n"),
            env_usize("PITLANE_EMBED_MAX_CHARS", DEFAULT_MAX_CHARS),
        );
    }

    let relative = project_root
        .and_then(|root| sym.file.strip_prefix(root).ok())
        .unwrap_or(sym.file.as_ref())
        .to_string_lossy()
        .replace('\\', "/");
    let source = if profile == DocumentProfile::MetadataCode {
        read_symbol_source(sym, false).ok()
    } else {
        None
    };

    let mut text = format!(
        "File: {relative}\nName: {}\nSymbol: {}\nType: {}\nLanguage: {}",
        sym.name, sym.qualified, sym.kind, sym.language
    );
    if let Some(signature) = &sym.signature {
        text.push_str("\nSignature:\n");
        text.push_str(signature);
    }
    if let Some(doc) = &sym.doc {
        text.push_str("\nDocumentation:\n");
        text.push_str(doc);
    }
    if let Some(source) = source {
        let ids = identifiers(
            &source,
            env_usize("PITLANE_EMBED_MAX_IDENTIFIERS", DEFAULT_IDENTIFIERS),
        );
        if !ids.is_empty() {
            text.push_str("\nIdentifiers:\n");
            text.push_str(&ids.join(" "));
        }
        text.push_str("\nCode:\n");
        text.push_str(&compact_code(
            &source,
            env_usize("PITLANE_EMBED_BODY_CHARS", DEFAULT_BODY_CHARS),
        ));
    }
    truncate_chars(
        &text,
        env_usize("PITLANE_EMBED_MAX_CHARS", DEFAULT_MAX_CHARS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::{Language, SymbolKind};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn symbol(file: Arc<PathBuf>) -> Symbol {
        Symbol {
            id: "src/cache.rs::allocate#function".into(),
            name: "allocate".into(),
            qualified: "kv_cache::allocate".into(),
            kind: SymbolKind::Function,
            language: Language::Rust,
            file,
            byte_start: 0,
            byte_end: 42,
            line_start: 1,
            line_end: 1,
            signature: Some("fn allocate(buffer_size: usize)".into()),
            doc: Some("Allocate the KV cache buffer.".into()),
        }
    }

    #[test]
    fn metadata_document_names_the_file_kind_and_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("src/cache.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            "fn allocate(buffer_size: usize) { reserve_kv(buffer_size); }",
        )
        .unwrap();
        let mut sym = symbol(Arc::new(file));
        sym.byte_end = std::fs::metadata(sym.file.as_ref()).unwrap().len() as usize;
        let doc = build_symbol_document(&sym, Some(dir.path()));
        assert!(doc.contains("File: src/cache.rs"));
        assert!(doc.contains("Symbol: kv_cache::allocate"));
        assert!(doc.contains("Type: function"));
        assert!(doc.contains("reserve_kv"));
    }
}
