use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;

use serde_json::{json, Value};

use crate::error::ToolError;
use crate::index::format::index_dir;
use crate::indexer::language::{Language, SymbolKind};
use crate::tools::index_project::load_project_index;

/// Build the set of character trigrams for `s` (lowercased).
fn trigrams(s: &str) -> HashSet<[char; 3]> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return HashSet::new();
    }
    chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

/// Jaccard similarity over trigrams: |A ∩ B| / |A ∪ B|. Returns 0.0 for very short strings.
fn trigram_similarity(a: &str, b: &str) -> f32 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let intersection = ta.intersection(&tb).count();
    let union = ta.len() + tb.len() - intersection;
    intersection as f32 / union as f32
}

pub struct SearchSymbolsParams {
    pub project: String,
    pub query: String,
    pub kind: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// "bm25" (default), "exact", "fuzzy", or "semantic" (reserved)
    pub mode: Option<String>,
}

pub async fn search_symbols(params: SearchSymbolsParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);

    let explicit_mode = params.mode.as_deref();
    let mode = explicit_mode.unwrap_or("bm25");

    match mode {
        "exact" | "fuzzy" | "bm25" => {}
        "semantic" => {
            return Err(anyhow::anyhow!(
                "Semantic search is not yet available. It will be enabled in a future version."
            ));
        }
        other => {
            return Err(ToolError::InvalidArgument {
                param: "mode".to_string(),
                message: format!("Unknown mode '{}'. Supported: bm25, exact, fuzzy", other),
            }
            .into());
        }
    }

    // Parse filters — shared across all modes.
    let kind_filter: Option<SymbolKind> = params
        .kind
        .as_deref()
        .map(|k| {
            SymbolKind::from_str(k).map_err(|_| ToolError::InvalidArgument {
                param: "kind".to_string(),
                message: format!(
                    "Unknown kind '{}'. Supported: function, method, struct, enum, trait, impl, mod, macro, const, type_alias, class, interface",
                    k
                ),
            })
        })
        .transpose()?;

    let lang_filter: Option<Language> = params
        .language
        .as_deref()
        .map(|l| match l.to_lowercase().as_str() {
            "rust" => Ok(Language::Rust),
            "python" => Ok(Language::Python),
            "javascript" | "js" => Ok(Language::JavaScript),
            "typescript" | "ts" => Ok(Language::TypeScript),
            "c" => Ok(Language::C),
            "cpp" | "c++" => Ok(Language::Cpp),
            "go" => Ok(Language::Go),
            "java" => Ok(Language::Java),
            "bash" | "sh" => Ok(Language::Bash),
            "csharp" | "c#" | "cs" => Ok(Language::CSharp),
            "ruby" | "rb" => Ok(Language::Ruby),
            "swift" => Ok(Language::Swift),
            "objc" | "objective-c" | "objectivec" => Ok(Language::ObjC),
            "kotlin" | "kt" => Ok(Language::Kotlin),
            "php" => Ok(Language::Php),
            "zig" => Ok(Language::Zig),
            "luau" | "lua" => Ok(Language::Lua),
            other => Err(ToolError::InvalidArgument {
                param: "language".to_string(),
                message: format!(
                    "Unknown language '{}'. Supported: rust, python, javascript, typescript, c, cpp, go, java, bash, csharp, ruby, swift, objc, php, zig, kotlin, lua",
                    other
                ),
            }),
        })
        .transpose()?;

    let file_glob = params
        .file
        .as_deref()
        .map(|f| {
            globset::GlobBuilder::new(f)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()?;

    // ------------------------------------------------------------------
    // BM25 path — ranked full-text search over name, qualified, signature,
    // and doc fields. Falls back silently to exact when tantivy isn't ready
    // (e.g. first call after upgrade) and the mode wasn't set explicitly.
    // ------------------------------------------------------------------
    if mode == "bm25" {
        let project_path = Path::new(&params.project);
        let canonical = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.to_path_buf());

        let bm25_result: anyhow::Result<Value> = (|| {
            let tantivy_dir = index_dir(&canonical)?.join("tantivy");
            crate::index::bm25::ensure(&index.symbols, &tantivy_dir)?;

            let has_glob = file_glob.is_some();
            // Fetch one extra to detect whether more results exist beyond this page.
            let fetch = if has_glob {
                (offset + limit) * 10 + 50
            } else {
                offset + limit + 1
            };

            let ids = crate::index::bm25::search(
                &params.query,
                &canonical,
                &tantivy_dir,
                kind_filter.as_ref(),
                lang_filter.as_ref(),
                fetch.max(1),
            )?;

            let mut results: Vec<Value> = Vec::new();
            let mut skipped = 0usize;
            let mut truncated = false;
            for id in ids {
                let sym = match index.symbols.get(&id) {
                    Some(s) => s,
                    None => continue,
                };
                // Kind/lang filters are already applied inside tantivy; re-check
                // here as a safety net in case the tantivy index is slightly stale.
                if let Some(ref kf) = kind_filter {
                    if &sym.kind != kf {
                        continue;
                    }
                }
                if let Some(ref lf) = lang_filter {
                    if &sym.language != lf {
                        continue;
                    }
                }
                if let Some(ref matcher) = file_glob {
                    let file_str = sym.file.to_string_lossy();
                    let file_path: &Path = file_str.as_ref().as_ref();
                    if !matcher.is_match(file_path) {
                        continue;
                    }
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if results.len() >= limit {
                    truncated = true;
                    break;
                }
                results.push(json!({
                    "id": sym.id,
                    "name": sym.name,
                    "qualified": sym.qualified,
                    "kind": sym.kind.to_string(),
                    "language": sym.language.to_string(),
                    "file": sym.file.to_string_lossy().replace('\\', "/"),
                    "line_start": sym.line_start,
                    "line_end": sym.line_end,
                    "signature": sym.signature,
                }));
            }

            let mut resp = json!({
                "results": results,
                "count": results.len(),
                "query": params.query,
                "truncated": truncated,
            });
            if truncated {
                resp["next_page_message"] = json!(format!(
                    "More results available. Call again with offset: {}",
                    offset + limit
                ));
            }
            Ok(resp)
        })();

        match bm25_result {
            Ok(v) => return Ok(v),
            Err(e) if explicit_mode.is_none() => {
                tracing::debug!(error = %e, "BM25 search failed, falling back to exact");
                // Fall through to exact below.
            }
            Err(e) => return Err(e),
        }
    }

    // ------------------------------------------------------------------
    // Exact path — substring match on name and qualified name.
    // Used directly when mode="exact", and as the BM25 fallback.
    // ------------------------------------------------------------------
    if mode == "exact" || mode == "bm25" {
        let query_lower = params.query.to_lowercase();
        let mut candidates = Vec::new();

        for sym in index.symbols.values() {
            let name_lower = sym.name.to_lowercase();
            let qualified_lower = sym.qualified.to_lowercase();
            if !name_lower.contains(&query_lower) && !qualified_lower.contains(&query_lower) {
                continue;
            }
            if let Some(ref kf) = kind_filter {
                if &sym.kind != kf {
                    continue;
                }
            }
            if let Some(ref lf) = lang_filter {
                if &sym.language != lf {
                    continue;
                }
            }
            if let Some(ref matcher) = file_glob {
                let file_str = sym.file.to_string_lossy();
                let file_path: &Path = file_str.as_ref().as_ref();
                if !matcher.is_match(file_path) {
                    continue;
                }
            }
            candidates.push(json!({
                "id": sym.id,
                "name": sym.name,
                "qualified": sym.qualified,
                "kind": sym.kind.to_string(),
                "language": sym.language.to_string(),
                "file": sym.file.to_string_lossy().replace('\\', "/"),
                "line_start": sym.line_start,
                "line_end": sym.line_end,
                "signature": sym.signature,
            }));
            if candidates.len() > offset + limit {
                break;
            }
        }

        let truncated = candidates.len() > offset + limit;
        let page: Vec<_> = candidates.into_iter().skip(offset).take(limit).collect();
        let mut resp = json!({
            "results": page,
            "count": page.len(),
            "query": params.query,
            "truncated": truncated,
        });
        if truncated {
            resp["next_page_message"] = json!(format!(
                "More results available. Call again with offset: {}",
                offset + limit
            ));
        }
        return Ok(resp);
    }

    // ------------------------------------------------------------------
    // Fuzzy path — trigram Jaccard similarity, ranked by score.
    // Explicit opt-in via mode="fuzzy".
    // ------------------------------------------------------------------
    let query_lower = params.query.to_lowercase();
    const FUZZY_THRESHOLD: f32 = 0.2;

    let mut scored: Vec<(f32, Value)> = index
        .symbols
        .values()
        .filter(|sym| {
            if let Some(ref kf) = kind_filter {
                if &sym.kind != kf {
                    return false;
                }
            }
            if let Some(ref lf) = lang_filter {
                if &sym.language != lf {
                    return false;
                }
            }
            if let Some(ref matcher) = file_glob {
                let file_str = sym.file.to_string_lossy();
                let file_path: &Path = file_str.as_ref().as_ref();
                if !matcher.is_match(file_path) {
                    return false;
                }
            }
            true
        })
        .filter_map(|sym| {
            let name_lower = sym.name.to_lowercase();
            let qualified_lower = sym.qualified.to_lowercase();
            let score = trigram_similarity(&query_lower, &name_lower)
                .max(trigram_similarity(&query_lower, &qualified_lower));
            if score >= FUZZY_THRESHOLD {
                Some((
                    score,
                    json!({
                        "id": sym.id,
                        "name": sym.name,
                        "qualified": sym.qualified,
                        "kind": sym.kind.to_string(),
                        "language": sym.language.to_string(),
                        "file": sym.file.to_string_lossy().replace('\\', "/"),
                        "line_start": sym.line_start,
                        "line_end": sym.line_end,
                        "signature": sym.signature,
                    }),
                ))
            } else {
                None
            }
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let page: Vec<_> = scored
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, v)| v)
        .collect();

    Ok(json!({
        "results": page,
        "count": page.len(),
        "query": params.query,
        "truncated": false,
        "fuzzy": true,
        "fuzzy_note": "Results are fuzzy-matched by trigram similarity and ranked by score.",
    }))
}
