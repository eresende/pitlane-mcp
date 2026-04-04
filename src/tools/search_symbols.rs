use std::collections::HashSet;
use std::str::FromStr;

use serde_json::{json, Value};

use crate::error::ToolError;
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
}

pub async fn search_symbols(params: SearchSymbolsParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);
    let query_lower = params.query.to_lowercase();

    // Parse optional filters
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
            "luau" | "lua" => Ok(Language::Luau),
            other => Err(ToolError::InvalidArgument {
                param: "language".to_string(),
                message: format!(
                    "Unknown language '{}'. Supported: rust, python, javascript, typescript, c, cpp, go, java, bash, csharp, ruby, swift, objc, php, zig, kotlin, luau",
                    other
                ),
            }),
        })
        .transpose()?;

    // File glob filter
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

    let mut candidates = Vec::new();

    for sym in index.symbols.values() {
        // Apply query filter
        let name_lower = sym.name.to_lowercase();
        let qualified_lower = sym.qualified.to_lowercase();
        if !name_lower.contains(&query_lower) && !qualified_lower.contains(&query_lower) {
            continue;
        }

        // Apply kind filter
        if let Some(ref kf) = kind_filter {
            if &sym.kind != kf {
                continue;
            }
        }

        // Apply language filter
        if let Some(ref lf) = lang_filter {
            if &sym.language != lf {
                continue;
            }
        }

        // Apply file filter
        if let Some(ref matcher) = file_glob {
            let file_str = sym.file.to_string_lossy();
            let file_path: &std::path::Path = file_str.as_ref().as_ref();
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

    // If substring search found nothing, fall back to trigram fuzzy matching.
    // Scan all symbols, score each by Jaccard trigram similarity, keep those
    // above a minimum threshold, and return them ranked by score.
    if candidates.is_empty() {
        const FUZZY_THRESHOLD: f32 = 0.2;
        let query_lower = params.query.to_lowercase();

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
                    let file_path: &std::path::Path = file_str.as_ref().as_ref();
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

        let fuzzy_page: Vec<_> = scored
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(_, v)| v)
            .collect();

        return Ok(json!({
            "results": fuzzy_page,
            "count": fuzzy_page.len(),
            "query": params.query,
            "truncated": false,
            "fuzzy": true,
            "fuzzy_note": "No exact substring match found; results are fuzzy-matched by trigram similarity and ranked by score.",
        }));
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
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ToolError;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, Indexer};
    use tempfile::TempDir;

    async fn setup_project(dir: &TempDir) -> String {
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_invalid_language_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
        let project = setup_project(&dir).await;

        let err = search_symbols(SearchSymbolsParams {
            project,
            query: "foo".to_string(),
            kind: None,
            language: Some("cobol".to_string()),
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        assert!(tool_err.to_string().contains("language"));
        assert!(tool_err.to_string().contains("cobol"));
    }

    #[tokio::test]
    async fn test_invalid_kind_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
        let project = setup_project(&dir).await;

        let err = search_symbols(SearchSymbolsParams {
            project,
            query: "foo".to_string(),
            kind: Some("widget".to_string()),
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        assert!(tool_err.to_string().contains("kind"));
        assert!(tool_err.to_string().contains("widget"));
    }

    #[tokio::test]
    async fn test_unindexed_project_returns_structured_error() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().to_string_lossy().to_string();
        let canonical = dir.path().canonicalize().unwrap();
        crate::cache::invalidate(&canonical);

        let err = search_symbols(SearchSymbolsParams {
            project: project.clone(),
            query: "foo".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "PROJECT_NOT_INDEXED");
        assert_eq!(tool_err.hint(), "Call index_project first.");
    }

    // ── Preservation tests (Property 2) ─────────────────────────────────────
    //
    // These tests MUST PASS on unfixed code. They establish the baseline
    // behavior that must not regress after the fix is applied in task 3.
    //
    // Validates: Requirements 3.1, 3.2, 3.5, 3.6

    /// Preservation: index 5 symbols, call with limit=20.
    /// All 5 must be returned (no truncation occurs when total < limit).
    ///
    /// Validates: Requirements 3.2
    #[tokio::test]
    async fn test_preserve_under_limit_returns_all() {
        let dir = TempDir::new().unwrap();

        // Write exactly 5 distinct Rust functions.
        let src = "pub fn alpha() {}\npub fn beta() {}\npub fn gamma() {}\npub fn delta() {}\npub fn epsilon() {}\n";
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();

        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: None,
        })
        .await
        .unwrap();

        let count = response["count"].as_u64().expect("count must be present");
        assert_eq!(
            count, 5,
            "all 5 symbols must be returned when total < limit"
        );

        let results = response["results"]
            .as_array()
            .expect("results must be array");
        assert_eq!(results.len(), 5, "results array must contain all 5 symbols");
    }

    /// Preservation: index 10 symbols, call twice with identical params.
    /// Both calls must return the same results (determinism).
    ///
    /// Validates: Requirements 3.1
    #[tokio::test]
    async fn test_preserve_first_page_results_stable() {
        let dir = TempDir::new().unwrap();

        // Write 10 distinct Rust functions.
        let mut src = String::new();
        for i in 0..10 {
            src.push_str(&format!("pub fn stable_{i}() {{}}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();

        let project = setup_project(&dir).await;

        let params = || SearchSymbolsParams {
            project: project.clone(),
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: None,
        };

        let resp1 = search_symbols(params()).await.unwrap();
        let resp2 = search_symbols(params()).await.unwrap();

        // Both calls must return the same count.
        assert_eq!(
            resp1["count"], resp2["count"],
            "count must be identical across repeated calls"
        );

        // Both calls must return the same symbol IDs in the same order.
        let ids1: Vec<&str> = resp1["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap_or(""))
            .collect();
        let ids2: Vec<&str> = resp2["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap_or(""))
            .collect();

        assert_eq!(
            ids1, ids2,
            "result IDs must be identical across repeated calls"
        );
    }

    /// Preservation: error paths (INVALID_ARGUMENT, PROJECT_NOT_INDEXED) are unchanged.
    ///
    /// These are the same error-path tests that already exist above, grouped here
    /// as explicit preservation tests to document that they must continue to pass
    /// after the fix is applied.
    ///
    /// Validates: Requirements 3.5, 3.6
    #[tokio::test]
    async fn test_preserve_error_paths_unchanged() {
        // --- INVALID_ARGUMENT: bad language ---
        {
            let dir = TempDir::new().unwrap();
            std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
            let project = setup_project(&dir).await;

            let err = search_symbols(SearchSymbolsParams {
                project,
                query: "foo".to_string(),
                kind: None,
                language: Some("cobol".to_string()),
                file: None,
                limit: None,
                offset: None,
            })
            .await
            .unwrap_err();

            let tool_err = err.downcast_ref::<ToolError>().expect("must be ToolError");
            assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        }

        // --- INVALID_ARGUMENT: bad kind ---
        {
            let dir = TempDir::new().unwrap();
            std::fs::write(dir.path().join("lib.rs"), b"pub fn foo() {}").unwrap();
            let project = setup_project(&dir).await;

            let err = search_symbols(SearchSymbolsParams {
                project,
                query: "foo".to_string(),
                kind: Some("widget".to_string()),
                language: None,
                file: None,
                limit: None,
                offset: None,
            })
            .await
            .unwrap_err();

            let tool_err = err.downcast_ref::<ToolError>().expect("must be ToolError");
            assert_eq!(tool_err.code(), "INVALID_ARGUMENT");
        }

        // --- PROJECT_NOT_INDEXED ---
        {
            let dir = TempDir::new().unwrap();
            let project = dir.path().to_string_lossy().to_string();
            let canonical = dir.path().canonicalize().unwrap();
            crate::cache::invalidate(&canonical);

            let err = search_symbols(SearchSymbolsParams {
                project,
                query: "foo".to_string(),
                kind: None,
                language: None,
                file: None,
                limit: None,
                offset: None,
            })
            .await
            .unwrap_err();

            let tool_err = err.downcast_ref::<ToolError>().expect("must be ToolError");
            assert_eq!(tool_err.code(), "PROJECT_NOT_INDEXED");
            assert_eq!(tool_err.hint(), "Call index_project first.");
        }
    }

    // ── Bug condition exploration tests ─────────────────────────────────────
    //
    // These tests MUST FAIL on unfixed code. Failure confirms the bug exists.
    // DO NOT fix the code or the tests when they fail — that is the expected outcome.
    //
    // Counterexamples documented here (observed on unfixed code):
    //   - `truncated` key is absent from the response object
    //   - `next_page_message` key is absent from the response object
    //   - The response only contains: { "results": [...], "count": 20, "query": "" }
    //
    // These tests encode the EXPECTED (fixed) behavior and will pass once the fix
    // is implemented in task 3.

    /// Bug condition: index 25 symbols, call search_symbols with limit=20.
    /// The response MUST contain `truncated: true` and `next_page_message`.
    /// FAILS on unfixed code because neither field is emitted.
    ///
    /// Validates: Requirements 1.1, 1.2, 1.5 (bug condition)
    #[tokio::test]
    async fn test_bug_search_symbols_truncation_signal_absent() {
        let dir = TempDir::new().unwrap();

        // Write 25 distinct Rust functions so the indexer finds exactly 25 symbols.
        let mut src = String::new();
        for i in 0..25 {
            src.push_str(&format!("pub fn sym_{i}() {{}}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();

        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: None,
        })
        .await
        .unwrap();

        // Sanity check: we got 20 results (the limit), confirming truncation occurred.
        assert_eq!(
            response["count"], 20,
            "expected 20 results (limit), got: {}",
            response["count"]
        );

        // BUG CONDITION: these assertions FAIL on unfixed code because the fields
        // are absent. When the fix is applied they will pass.
        assert_eq!(
            response["truncated"], true,
            "COUNTEREXAMPLE: `truncated` field is absent from response — \
             caller cannot detect that 5 more symbols exist. \
             Full response: {}",
            response
        );
        assert!(
            response["next_page_message"].is_string(),
            "COUNTEREXAMPLE: `next_page_message` field is absent from response — \
             caller has no hint on how to retrieve the next page. \
             Full response: {}",
            response
        );
    }

    // ── Pagination unit tests (Task 3.5) ────────────────────────────────────

    /// 25 symbols, limit=20 → truncated:true, next_page_message contains "offset: 20"
    #[tokio::test]
    async fn test_search_symbols_truncated_true_with_message() {
        let dir = TempDir::new().unwrap();
        let mut src = String::new();
        for i in 0..25 {
            src.push_str(&format!("pub fn sym_{i:02}() {{}}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(response["truncated"], true, "truncated must be true");
        let msg = response["next_page_message"]
            .as_str()
            .expect("next_page_message must be present");
        assert!(
            msg.contains("offset: 20"),
            "message must contain 'offset: 20', got: {msg}"
        );
    }

    /// 25 symbols, offset=20, limit=20 → count=5, truncated:false
    #[tokio::test]
    async fn test_search_symbols_offset_returns_second_page() {
        let dir = TempDir::new().unwrap();
        let mut src = String::new();
        for i in 0..25 {
            src.push_str(&format!("pub fn sym_{i:02}() {{}}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: Some(20),
        })
        .await
        .unwrap();

        assert_eq!(response["count"], 5, "second page must have 5 items");
        assert_eq!(
            response["truncated"], false,
            "truncated must be false on last page"
        );
        assert!(
            response["next_page_message"].is_null(),
            "no next_page_message on last page"
        );
    }

    /// 5 symbols, limit=20 → truncated:false, no next_page_message
    #[tokio::test]
    async fn test_search_symbols_under_limit_truncated_false() {
        let dir = TempDir::new().unwrap();
        let src = "pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\npub fn e() {}\n";
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(response["truncated"], false, "truncated must be false");
        assert!(
            response["next_page_message"].is_null(),
            "no next_page_message when not truncated"
        );
    }

    /// 5 symbols, offset=100 → count=0, truncated:false
    #[tokio::test]
    async fn test_search_symbols_offset_beyond_total_empty() {
        let dir = TempDir::new().unwrap();
        let src = "pub fn a() {}\npub fn b() {}\npub fn c() {}\npub fn d() {}\npub fn e() {}\n";
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
        let project = setup_project(&dir).await;

        let response = search_symbols(SearchSymbolsParams {
            project,
            query: String::new(),
            kind: None,
            language: None,
            file: None,
            limit: Some(20),
            offset: Some(100),
        })
        .await
        .unwrap();

        assert_eq!(
            response["count"], 0,
            "count must be 0 when offset beyond total"
        );
        assert_eq!(response["truncated"], false, "truncated must be false");
    }

    // ── Property-based tests (Task 3.7) ─────────────────────────────────────
    //
    // Manual parametric loop approach (proptest not available as a dependency).
    //
    // Validates: Requirements 2.1, 2.2, 2.3, 2.4, 2.5, 2.6, 3.1, 3.2, 3.3

    /// Property 1: for a range of (total, limit, offset) values:
    ///   response.count == min(max(total - offset, 0), limit)
    ///   truncated == (offset + response.count < total)
    ///
    /// **Validates: Requirements 2.1, 2.2, 2.6**
    #[tokio::test]
    async fn test_property_count_and_truncated_formula() {
        let cases: &[(usize, usize, usize)] = &[
            (0, 10, 0),
            (5, 10, 0),
            (10, 10, 0),
            (15, 10, 0),
            (15, 10, 5),
            (15, 10, 10),
            (15, 10, 15),
            (15, 10, 20),
            (30, 10, 0),
            (30, 10, 10),
            (30, 10, 20),
            (30, 10, 30),
            (1, 1, 0),
            (1, 1, 1),
            (25, 20, 0),
            (25, 20, 20),
            (25, 20, 25),
        ];

        for &(total, limit, offset) in cases {
            let dir = TempDir::new().unwrap();
            let mut src = String::new();
            for i in 0..total {
                src.push_str(&format!("pub fn sym_{i:04}() {{}}\n"));
            }
            if src.is_empty() {
                src.push_str("// empty\n");
            }
            std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
            let project = setup_project(&dir).await;

            let response = search_symbols(SearchSymbolsParams {
                project,
                query: String::new(),
                kind: None,
                language: None,
                file: None,
                limit: Some(limit),
                offset: Some(offset),
            })
            .await
            .unwrap();

            let expected_count = total.saturating_sub(offset).min(limit);
            let actual_count = response["count"].as_u64().expect("count must be present") as usize;
            assert_eq!(
                actual_count, expected_count,
                "total={total}, limit={limit}, offset={offset}: expected count={expected_count}, got {actual_count}"
            );

            let expected_truncated = offset + actual_count < total;
            let actual_truncated = response["truncated"]
                .as_bool()
                .expect("truncated must be bool");
            assert_eq!(
                actual_truncated, expected_truncated,
                "total={total}, limit={limit}, offset={offset}: expected truncated={expected_truncated}, got {actual_truncated}"
            );

            if actual_truncated {
                assert!(
                    response["next_page_message"].is_string(),
                    "total={total}, limit={limit}, offset={offset}: next_page_message must be present when truncated"
                );
            } else {
                assert!(
                    response["next_page_message"].is_null(),
                    "total={total}, limit={limit}, offset={offset}: next_page_message must be absent when not truncated"
                );
            }
        }
    }

    /// Property 2: for total <= limit, truncated:false and count==total
    ///
    /// **Validates: Requirements 2.6, 3.2**
    #[tokio::test]
    async fn test_property_under_limit_no_truncation() {
        let cases: &[(usize, usize)] = &[(0, 1), (1, 1), (5, 10), (10, 10), (19, 20), (20, 20)];

        for &(total, limit) in cases {
            let dir = TempDir::new().unwrap();
            let mut src = String::new();
            for i in 0..total {
                src.push_str(&format!("pub fn sym_{i:04}() {{}}\n"));
            }
            if src.is_empty() {
                src.push_str("// empty\n");
            }
            std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
            let project = setup_project(&dir).await;

            let response = search_symbols(SearchSymbolsParams {
                project,
                query: String::new(),
                kind: None,
                language: None,
                file: None,
                limit: Some(limit),
                offset: None,
            })
            .await
            .unwrap();

            let actual_count = response["count"].as_u64().expect("count must be present") as usize;
            assert_eq!(
                actual_count, total,
                "total={total}, limit={limit}: expected count={total}, got {actual_count}"
            );
            assert_eq!(
                response["truncated"], false,
                "total={total}, limit={limit}: truncated must be false"
            );
        }
    }

    /// Full pagination walk: total=30, limit=10, walk offset=0,10,20.
    /// All 30 symbols must appear exactly once across the 3 pages.
    ///
    /// **Validates: Requirements 2.2, 3.1, 3.2**
    #[tokio::test]
    async fn test_property_full_pagination_walk() {
        let total = 30usize;
        let limit = 10usize;

        let dir = TempDir::new().unwrap();
        let mut src = String::new();
        for i in 0..total {
            src.push_str(&format!("pub fn sym_{i:04}() {{}}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();
        let project = setup_project(&dir).await;

        let mut all_ids: Vec<String> = Vec::new();
        let mut offset = 0;
        loop {
            let response = search_symbols(SearchSymbolsParams {
                project: project.clone(),
                query: String::new(),
                kind: None,
                language: None,
                file: None,
                limit: Some(limit),
                offset: Some(offset),
            })
            .await
            .unwrap();

            let page = response["results"]
                .as_array()
                .expect("results must be array");
            for item in page {
                all_ids.push(item["id"].as_str().unwrap_or("").to_string());
            }

            let truncated = response["truncated"]
                .as_bool()
                .expect("truncated must be bool");
            if !truncated {
                break;
            }
            offset += limit;
        }

        assert_eq!(
            all_ids.len(),
            total,
            "all {total} symbols must be returned across pages"
        );

        // No duplicates
        let mut sorted = all_ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), total, "no duplicate symbols across pages");
    }

    /// Fuzzy fallback triggers when the query has no substring match.
    #[tokio::test]
    async fn test_fuzzy_fallback_on_no_substring_match() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn get_file_outline() {}\npub fn search_symbols() {}",
        )
        .unwrap();
        let project = setup_project(&dir).await;

        // "get_fiel_outline" has a transposition — no substring match, should fuzzy-match.
        let result = search_symbols(SearchSymbolsParams {
            project,
            query: "get_fiel_outline".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(result["fuzzy"], true, "should fall back to fuzzy mode");
        assert!(
            result["count"].as_u64().unwrap() > 0,
            "fuzzy search should find get_file_outline"
        );
        let names: Vec<_> = result["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"get_file_outline"),
            "get_file_outline should be in fuzzy results, got: {names:?}"
        );
    }

    /// Exact substring match does NOT set fuzzy flag.
    #[tokio::test]
    async fn test_exact_match_does_not_set_fuzzy_flag() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn get_file_outline() {}").unwrap();
        let project = setup_project(&dir).await;

        let result = search_symbols(SearchSymbolsParams {
            project,
            query: "get_file_outline".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert!(
            result["fuzzy"].is_null(),
            "exact match should not set fuzzy flag"
        );
        assert_eq!(result["count"].as_u64().unwrap(), 1);
    }

    /// Completely unrelated query returns empty results (below threshold), not fuzzy garbage.
    #[tokio::test]
    async fn test_fuzzy_below_threshold_returns_empty() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn get_file_outline() {}").unwrap();
        let project = setup_project(&dir).await;

        let result = search_symbols(SearchSymbolsParams {
            project,
            query: "zzzzzzzzzz".to_string(),
            kind: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(
            result["count"].as_u64().unwrap(),
            0,
            "completely unrelated query should return no results"
        );
    }

    /// trigram_similarity unit tests.
    #[test]
    fn test_trigram_similarity_identical() {
        assert!((trigram_similarity("hello", "hello") - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_trigram_similarity_one_char_off() {
        let score = trigram_similarity("get_symbol", "get_symbo");
        assert!(score > 0.5, "one char off should score > 0.5, got {score}");
    }

    #[test]
    fn test_trigram_similarity_unrelated() {
        let score = trigram_similarity("abcdef", "xyz123");
        assert!(
            score < 0.2,
            "unrelated strings should score < 0.2, got {score}"
        );
    }

    #[test]
    fn test_trigram_similarity_short_string() {
        // Strings shorter than 3 chars produce empty trigram sets → 0.0
        assert_eq!(trigram_similarity("ab", "abc"), 0.0);
    }
}
