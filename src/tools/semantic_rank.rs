use std::collections::HashSet;

use crate::index::repo_profile::PathRole;
use crate::indexer::language::{Symbol, SymbolKind};

#[derive(Debug, Clone, Copy)]
pub struct HybridWeights {
    pub lexical: f32,
    pub bm25: f32,
    pub test_penalty: f32,
    pub auxiliary_penalty: f32,
    pub implementation_kind: f32,
    pub session: f32,
}

impl HybridWeights {
    pub fn from_env() -> Self {
        Self {
            lexical: env_f32("PITLANE_SEMANTIC_LEXICAL_WEIGHT", 0.10),
            bm25: env_f32("PITLANE_SEMANTIC_BM25_WEIGHT", 0.03),
            test_penalty: env_f32("PITLANE_SEMANTIC_TEST_PENALTY", 0.12),
            auxiliary_penalty: env_f32("PITLANE_SEMANTIC_AUXILIARY_PENALTY", 0.03),
            implementation_kind: env_f32("PITLANE_SEMANTIC_KIND_WEIGHT", 0.01),
            // Cross-query session history made unrelated symbols sticky. It is opt-in now.
            session: env_f32("PITLANE_SEMANTIC_SESSION_WEIGHT", 0.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScoreBreakdown {
    pub raw_similarity: f32,
    pub lexical: f32,
    pub bm25: f32,
    pub path: f32,
    pub symbol_kind: f32,
    pub session: f32,
    pub final_score: f32,
}

fn env_f32(name: &str, default: f32) -> f32 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &f32| value.is_finite() && *value >= 0.0)
        .unwrap_or(default)
}

fn stem(token: &str) -> String {
    let mut value = token.to_ascii_lowercase();
    for suffix in [
        "ization", "ation", "ments", "ment", "ing", "ions", "ion", "ers", "er", "ed", "s",
    ] {
        if value.len() > suffix.len() + 3 && value.ends_with(suffix) {
            value.truncate(value.len() - suffix.len());
            break;
        }
    }
    value
}

fn tokens(text: &str) -> HashSet<String> {
    const STOP: &[&str] = &[
        "where",
        "what",
        "which",
        "are",
        "is",
        "the",
        "this",
        "that",
        "code",
        "source",
        "implemented",
        "implementation",
        "handle",
        "handled",
        "does",
        "with",
        "for",
        "from",
    ];
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(stem)
        .filter(|token| !STOP.contains(&token.as_str()))
        .collect()
}

pub fn bm25_query_terms(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "where",
        "what",
        "which",
        "are",
        "is",
        "the",
        "this",
        "that",
        "code",
        "source",
        "implemented",
        "implementation",
        "does",
        "with",
        "for",
        "from",
    ];
    let mut seen = HashSet::new();
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .map(str::to_ascii_lowercase)
        .filter(|term| term.len() >= 3 && !STOP.contains(&term.as_str()))
        .filter(|term| seen.insert(term.clone()))
        .take(8)
        .collect()
}

fn lexical_overlap(query: &str, sym: &Symbol) -> f32 {
    let query_tokens = tokens(query);
    if query_tokens.is_empty() {
        return 0.0;
    }
    let candidate = format!(
        "{} {} {} {} {}",
        sym.name,
        sym.qualified,
        sym.file.to_string_lossy(),
        sym.signature.as_deref().unwrap_or(""),
        sym.doc.as_deref().unwrap_or("")
    );
    let candidate_tokens = tokens(&candidate);
    let matched = query_tokens
        .iter()
        .filter(|query_token| {
            candidate_tokens.iter().any(|candidate_token| {
                query_token == &candidate_token
                    || (query_token.len() >= 4
                        && candidate_token.len() >= 4
                        && (query_token.starts_with(candidate_token)
                            || candidate_token.starts_with(query_token.as_str())))
            })
        })
        .count();
    matched as f32 / query_tokens.len() as f32
}

fn asks_for_tests(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("test") || lower.contains("example") || lower.contains("fixture")
}

fn asks_for_implementation(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("implement")
        || lower.contains("handle")
        || lower.contains("allocat")
        || lower.contains("where")
}

pub fn hybrid_score(
    raw_similarity: f32,
    query: &str,
    sym: &Symbol,
    path_role: PathRole,
    bm25_rank: Option<usize>,
    raw_session_boost: f32,
    weights: HybridWeights,
) -> ScoreBreakdown {
    let lexical = weights.lexical * lexical_overlap(query, sym);
    let bm25 = bm25_rank
        .map(|rank| weights.bm25 / (1.0 + rank as f32 / 10.0))
        .unwrap_or(0.0);
    let lower_path = sym.file.to_string_lossy().to_ascii_lowercase();
    let path = if asks_for_tests(query) {
        0.0
    } else if path_role == PathRole::Test {
        -weights.test_penalty
    } else if lower_path.contains("/examples/")
        || lower_path.contains("/example/")
        || lower_path.contains("/third_party/")
        || lower_path.contains("/third-party/")
        || lower_path.contains("/vendor/")
    {
        -weights.auxiliary_penalty
    } else {
        0.0
    };
    let symbol_kind = if asks_for_implementation(query)
        && matches!(sym.kind, SymbolKind::Function | SymbolKind::Method)
    {
        weights.implementation_kind
    } else {
        0.0
    };
    let session = weights.session * raw_session_boost;
    ScoreBreakdown {
        raw_similarity,
        lexical,
        bm25,
        path,
        symbol_kind,
        session,
        final_score: raw_similarity + lexical + bm25 + path + symbol_kind + session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::{Language, SymbolKind};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn symbol(file: &str, qualified: &str) -> Symbol {
        Symbol {
            id: format!("{file}::{qualified}#method"),
            name: qualified.rsplit("::").next().unwrap().into(),
            qualified: qualified.into(),
            kind: SymbolKind::Method,
            language: Language::Cpp,
            file: Arc::new(PathBuf::from(file)),
            byte_start: 0,
            byte_end: 1,
            line_start: 1,
            line_end: 1,
            signature: None,
            doc: None,
        }
    }

    #[test]
    fn lexical_and_path_signals_promote_production_kv_cache_code() {
        let weights = HybridWeights::from_env();
        let production = hybrid_score(
            0.70,
            "Where is KV cache allocation implemented?",
            &symbol("src/llama-kv-cache.cpp", "llama_kv_cache::llama_kv_cache"),
            PathRole::Unknown,
            Some(0),
            0.0,
            weights,
        );
        let test = hybrid_score(
            0.72,
            "Where is KV cache allocation implemented?",
            &symbol("tests/test-cache.cpp", "test_cache"),
            PathRole::Test,
            None,
            0.0,
            weights,
        );
        assert!(production.final_score > test.final_score);
    }
}
