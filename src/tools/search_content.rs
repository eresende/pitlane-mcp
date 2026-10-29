use std::path::{Path, PathBuf};

use anyhow::Context;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexBuilder;
use serde_json::{json, Value};
use walkdir::{DirEntry, WalkDir};

use crate::error::ToolError;
use crate::indexer::{
    default_exclude_patterns, extra_excluded_dir_names, is_declaration_file,
    is_excluded_dir_name_with_custom, is_supported_extension, load_gitignore_patterns,
};
use crate::path_policy::{read_regular_file, regular_file_metadata, resolve_project_path};
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{
    attach_steering, build_steering, guidance_enabled, take_fallback_candidates,
};

const DEFAULT_LIMIT: usize = 8;
const MAX_CONTEXT_LINES: usize = 5;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_REGEX_PATTERN_LEN: usize = 512;
const MAX_REGEX_COMPILE_SIZE: usize = 1 << 20;

pub struct SearchContentParams {
    pub project: String,
    pub query: String,
    pub regex: Option<bool>,
    pub case_sensitive: Option<bool>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub before_context: Option<usize>,
    pub after_context: Option<usize>,
}

pub async fn search_content(params: SearchContentParams) -> anyhow::Result<Value> {
    if params.query.trim().is_empty() {
        return Err(ToolError::InvalidArgument {
            param: "query".to_string(),
            message: "query must not be empty".to_string(),
        }
        .into());
    }

    let canonical = resolve_project_path(&params.project)?;
    let _index = load_project_index(&params.project)?;

    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    let offset = params.offset.unwrap_or(0);
    // An unset `regex` flag used to force a literal interpretation, so a query
    // such as `flag.*parse|ParseFlags` could never match anything and the caller
    // had no signal that a pattern was searched as text. Only an explicit
    // `regex: false` keeps literal semantics now. `|` and `\b` are not enough
    // on their own to opt in: they appear in closures, `a|b`, and paths like
    // `C:\bin`.
    let mut regex = params.regex.unwrap_or(false);
    // An auto-detected pattern longer than the regex cap is searched literally
    // instead of raising the error reserved for an explicit `regex: true`.
    let mut auto_regex_over_length = false;
    let case_sensitive = params.case_sensitive.unwrap_or(false);
    let before_context = params.before_context.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let after_context = params.after_context.unwrap_or(0).min(MAX_CONTEXT_LINES);

    let file_glob = params
        .file
        .as_deref()
        .map(|f| build_file_filter(&canonical, f))
        .transpose()?;
    let language_filter = params
        .language
        .as_deref()
        .map(parse_language_filter)
        .transpose()?;
    let exclude_set = build_exclude_set(&canonical)?;
    let extra_excluded_dirs = extra_excluded_dir_names();

    let matcher = if params.regex == Some(false)
        || (params.regex.is_none() && !auto_regex_candidate(&params.query))
    {
        literal_matcher(&params.query, case_sensitive)
    } else if params.regex.is_none() && params.query.len() > MAX_REGEX_PATTERN_LEN {
        auto_regex_over_length = true;
        literal_matcher(&params.query, case_sensitive)
    } else if params.query.len() > MAX_REGEX_PATTERN_LEN {
        return Err(ToolError::InvalidArgument {
            param: "query".to_string(),
            message: format!(
                "regex pattern exceeds {MAX_REGEX_PATTERN_LEN} characters; narrow the pattern or use literal search"
            ),
        }
        .into());
    } else {
        let compiled = RegexBuilder::new(&params.query)
            .case_insensitive(!case_sensitive)
            .size_limit(MAX_REGEX_COMPILE_SIZE)
            .build();
        match compiled {
            // An auto-detected pattern that matches the empty string would
            // report every line as a hit at column 1. Keep the explicit
            // `regex: true` path unchanged, including that case.
            Ok(re) if params.regex.is_none() && re.is_match("") => {
                literal_matcher(&params.query, case_sensitive)
            }
            Ok(re) => {
                regex = true;
                ContentMatcher::Regex(re)
            }
            // The caller never asked for a regex, so degrade to a literal search
            // instead of rejecting a query we only guessed was a pattern. Record
            // the effective mode so the response never claims regex semantics it
            // did not use.
            Err(_) if params.regex.is_none() => literal_matcher(&params.query, case_sensitive),
            Err(e) => {
                return Err(ToolError::InvalidArgument {
                    param: "query".to_string(),
                    message: format!("invalid regex: {e}"),
                }
                .into())
            }
        }
    };

    let mut files = collect_searchable_files(
        &canonical,
        &exclude_set,
        &extra_excluded_dirs,
        file_glob.as_ref(),
        language_filter,
    )?;
    files.sort();

    let mut matches = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;

    'files: for path in files {
        let bytes = read_regular_file(&path)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let rel = rel_string(&canonical, &path);

        for (line_idx, line) in lines.iter().enumerate() {
            let Some(column) = matcher.find_column(line) else {
                continue;
            };

            if skipped < offset {
                skipped += 1;
                continue;
            }
            if matches.len() >= limit {
                truncated = true;
                break 'files;
            }

            let before_start = line_idx.saturating_sub(before_context);
            let after_end = (line_idx + after_context + 1).min(lines.len());

            matches.push(json!({
                "file": rel,
                "line": line_idx + 1,
                "column": column,
                "line_text": line,
                "before": lines[before_start..line_idx]
                    .iter()
                    .enumerate()
                    .map(|(idx, text)| json!({
                        "line": before_start + idx + 1,
                        "text": *text,
                    }))
                    .collect::<Vec<_>>(),
                "after": lines[line_idx + 1..after_end]
                    .iter()
                    .enumerate()
                    .map(|(idx, text)| json!({
                        "line": line_idx + idx + 2,
                        "text": *text,
                    }))
                    .collect::<Vec<_>>(),
            }));
        }
    }

    let steering_matches: Vec<Value> = matches.clone();
    let mut response = json!({
        "matches": matches,
        "count": matches.len(),
        "query": params.query,
        "regex": regex,
        "case_sensitive": case_sensitive,
        "truncated": truncated,
    });
    if truncated {
        response["next_page_message"] = json!(format!(
            "More matches available. Call again with offset: {}",
            offset + limit
        ));
    }
    let (zero_next_step, zero_why) = zero_result_copy(regex, &params.query, auto_regex_over_length);
    let mut guidance = json!({
        "next_step": if matches.is_empty() {
            zero_next_step
        } else {
            "Use the matched file paths to pivot to read_code_unit, locate_code, or trace_path instead of repeating nearby text searches.".to_string()
        },
        "avoid": "Avoid shell grep when search_content can search the indexed source files directly."
    });
    if auto_regex_over_length {
        guidance["regex_fallback"] = json!(format!(
            "Auto-detected regular expressions longer than {MAX_REGEX_PATTERN_LEN} characters are searched as literal text. Narrow the pattern and pass regex: true to force regex mode."
        ));
    }
    let steering = if matches.is_empty() {
        // Never recommend this tool with the same target again: a zero-result
        // search is the moment to change strategy, not to repeat it. The
        // recommendation must also name a tool that is always exposed, because
        // advanced primitive tools are hidden unless the host opts in.
        build_steering(
            0.2,
            zero_why,
            "locate_code",
            json!({ "project": params.project, "query": params.query }),
            take_fallback_candidates(&steering_matches),
        )
    } else {
        let top = &steering_matches[0];
        let document = top["file"].as_str().is_some_and(|file| {
            super::documents::is_document_extension(
                Path::new(file)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or(""),
            )
        });
        if document {
            guidance["next_step"] = json!("Use read_code_unit with file_path and line_start/line_end to inspect the match, or omit the line bounds for a heading/key outline.");
        }
        build_steering(
            0.86,
            "The matched line provides direct evidence for the requested text snippet.".to_string(),
            // `read_code_unit` is exposed at every tier; `get_file_outline` and
            // `search_symbols` are not, so steering must not depend on them.
            // `line_start` without `line_end` is rejected, so both bounds are
            // always set. `project` matches the zero-result target.
            "read_code_unit",
            json!({
                "project": params.project,
                "file_path": top["file"],
                "line_start": top["line"],
                "line_end": top["line"],
            }),
            take_fallback_candidates(&steering_matches),
        )
    };
    attach_steering(&mut response, steering);
    if guidance_enabled() {
        response["guidance"] = guidance;
    }
    Ok(response)
}

/// Regex syntax that suggests the caller meant a pattern rather than a literal.
///
/// A query such as `flag.*parse|ParseFlags|command.*line` is a common way to
/// guess at a renamed symbol. Searched literally it can never match, so a zero
/// result then looks like "this code does not exist" instead of "wrong strategy".
///
/// `|` and `\b` are deliberately absent. They occur in closures (`|x|`),
/// alternation-shaped text (`a|b`), and Windows paths (`C:\bin`). Treating
/// those misses as "retry with regex: true" recreates the false-positive hits.
const REGEX_SYNTAX_MARKERS: [&str; 4] = [".*", "(?:", "^", "["];

/// Markers strong enough to change how an unset `regex` flag is interpreted.
///
/// Deliberately narrower than the hint markers: `[`, `^` and `$` occur in
/// ordinary code snippets such as `arr[0]`, and `|` / `\b` occur in closures,
/// `a|b`, and paths such as `C:\bin`. Silently switching those to a regex
/// would change results the caller did not ask to change.
const REGEX_AUTODETECT_MARKERS: [&str; 2] = [".*", "(?:"];

fn looks_like_regex(query: &str) -> bool {
    REGEX_SYNTAX_MARKERS
        .iter()
        .any(|marker| query.contains(marker))
}

fn auto_regex_candidate(query: &str) -> bool {
    REGEX_AUTODETECT_MARKERS
        .iter()
        .any(|marker| query.contains(marker))
}

fn literal_matcher(query: &str, case_sensitive: bool) -> ContentMatcher {
    ContentMatcher::Literal {
        needle: query.to_string(),
        needle_folded: query.to_lowercase(),
        case_sensitive,
    }
}

/// Copy for a zero-result search.
///
/// Returns `(guidance.next_step, steering.why_this_matched)`.
fn zero_result_copy(regex: bool, query: &str, auto_regex_over_length: bool) -> (String, String) {
    if auto_regex_over_length {
        (
            format!(
                "The query looks like a regular expression but exceeds {MAX_REGEX_PATTERN_LEN} characters, so it was searched as a literal string. Narrow the pattern to {MAX_REGEX_PATTERN_LEN} characters or fewer and retry with regex: true, or use locate_code/investigate to find the subsystem by behavior."
            ),
            format!(
                "No literal match; the query looks like a regular expression but exceeds {MAX_REGEX_PATTERN_LEN} characters, so it was searched as text."
            ),
        )
    } else if regex {
        (
            "The query was searched as a regular expression and matched nothing. Use locate_code or investigate to find the subsystem by behavior instead of refining the pattern.".to_string(),
            "No regular-expression match was found, so this is a weak content search result.".to_string(),
        )
    } else if looks_like_regex(query) {
        (
            "The query contains regular-expression syntax but was searched as a literal string. Retry with regex: true, or use locate_code/investigate to find the subsystem by behavior instead of by name.".to_string(),
            "No literal match; the query looks like a regular expression but was searched as text.".to_string(),
        )
    } else {
        (
            "A text search only matches literal snippets. Use locate_code or investigate to find a subsystem by behavior or intent instead of guessing the symbol names this revision uses.".to_string(),
            "No direct text match was found, so this is a weak content search result.".to_string(),
        )
    }
}

pub(crate) fn build_file_filter(root: &Path, filter: &str) -> anyhow::Result<globset::GlobMatcher> {
    let path = Path::new(filter);
    let relative = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let pattern = if root.join(path).is_dir() {
        let directory = relative.trim_end_matches('/');
        if directory.is_empty() || directory == "." {
            "**".to_string()
        } else {
            format!("{directory}/**")
        }
    } else {
        relative.into_owned()
    };
    Ok(GlobBuilder::new(&pattern)
        .case_insensitive(true)
        .build()?
        .compile_matcher())
}

enum ContentMatcher {
    Literal {
        needle: String,
        needle_folded: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
}

impl ContentMatcher {
    fn find_column(&self, line: &str) -> Option<usize> {
        match self {
            Self::Literal {
                needle,
                needle_folded,
                case_sensitive,
            } => {
                let idx = if *case_sensitive {
                    line.find(needle)
                } else {
                    line.to_lowercase().find(needle_folded)
                }?;
                Some(idx + 1)
            }
            Self::Regex(re) => re.find(line).map(|m| m.start() + 1),
        }
    }
}

pub(crate) fn parse_language_filter(language: &str) -> anyhow::Result<&'static [&'static str]> {
    match language.to_lowercase().as_str() {
        "markdown" | "md" => Ok(&["md", "markdown"]),
        "json" => Ok(&["json"]),
        "yaml" | "yml" => Ok(&["yaml", "yml"]),
        "toml" => Ok(&["toml"]),
        "rust" => Ok(&["rs"]),
        "python" => Ok(&["py"]),
        "javascript" | "js" => Ok(&["js", "jsx", "mjs", "cjs"]),
        "typescript" | "ts" => Ok(&["ts", "tsx", "mts", "cts"]),
        "svelte" => Ok(&["svelte"]),
        "c" => Ok(&["c", "h"]),
        "cpp" | "c++" => Ok(&["cpp", "cc", "cxx", "hpp", "hxx"]),
        "go" => Ok(&["go"]),
        "java" => Ok(&["java"]),
        "bash" | "sh" => Ok(&["sh", "bash"]),
        "csharp" | "c#" | "cs" => Ok(&["cs"]),
        "ruby" | "rb" => Ok(&["rb"]),
        "swift" => Ok(&["swift"]),
        "objc" | "objective-c" | "objectivec" => Ok(&["m", "mm"]),
        "php" => Ok(&["php"]),
        "zig" => Ok(&["zig"]),
        "kotlin" | "kt" => Ok(&["kt", "kts"]),
        "luau" | "lua" => Ok(&["luau", "lua"]),
        "solidity" | "sol" => Ok(&["sol"]),
        other => Err(ToolError::InvalidArgument {
            param: "language".to_string(),
            message: format!(
                "Unknown language '{}'. Supported: rust, python, javascript, typescript, svelte, c, cpp, go, java, bash, csharp, ruby, swift, objc, php, zig, kotlin, lua, solidity, markdown, json, yaml, toml",
                other
            ),
        }
        .into()),
    }
}

pub(crate) fn build_exclude_set(root: &Path) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in default_exclude_patterns()
        .into_iter()
        .chain(load_gitignore_patterns(root))
        .chain(
            crate::index::format::load_project_meta(root)
                .map(|meta| meta.effective_excludes)
                .unwrap_or_default(),
        )
    {
        builder.add(globset::Glob::new(&pattern)?);
    }
    Ok(builder.build()?)
}

pub(crate) fn collect_searchable_files(
    root: &Path,
    exclude_set: &GlobSet,
    extra_excluded_dirs: &std::collections::HashSet<String>,
    file_glob: Option<&globset::GlobMatcher>,
    language_filter: Option<&[&str]>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(root, exclude_set, extra_excluded_dirs, entry))
    {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(metadata) = regular_file_metadata(path)? else {
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if (!is_supported_extension(ext) && !super::documents::is_document_extension(ext))
            || is_declaration_file(path)
        {
            continue;
        }
        if let Some(exts) = language_filter {
            if !exts.contains(&ext) {
                continue;
            }
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel.to_string_lossy();
        if rel.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| is_excluded_dir_name_with_custom(name, extra_excluded_dirs))
        }) {
            continue;
        }
        if exclude_set.is_match(rel_str.as_ref()) || exclude_set.is_match(path) {
            continue;
        }
        if let Some(matcher) = file_glob {
            let rel_path: &Path = rel_str.as_ref().as_ref();
            if !matcher.is_match(rel_path) {
                continue;
            }
        }
        files.push(path.to_path_buf());
    }
    Ok(files)
}

fn should_descend(
    root: &Path,
    exclude_set: &GlobSet,
    extra_excluded_dirs: &std::collections::HashSet<String>,
    entry: &DirEntry,
) -> bool {
    let path = entry.path();
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return true,
    };
    if rel == Path::new("") {
        return true;
    }
    let rel_str = rel.to_string_lossy();
    if exclude_set.is_match(rel_str.as_ref()) {
        return false;
    }
    if rel.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| is_excluded_dir_name_with_custom(name, extra_excluded_dirs))
    }) {
        return false;
    }
    if entry.file_type().is_dir() && exclude_set.is_match(format!("{rel_str}/").as_str()) {
        return false;
    }
    true
}

fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::format::{index_dir, save_index};
    use crate::indexer::{registry, test_env_var_lock, Indexer, EXCLUDE_DIRS_ENV_VAR};
    use crate::tools::orchestrator::{read_code_unit, ReadCodeUnitParams};
    use tempfile::TempDir;

    fn setup_indexed_project(dir: &TempDir) -> String {
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        crate::cache::invalidate(&canonical);
        dir.path().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_search_content_literal_finds_supported_source() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn hello_world() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_content(SearchContentParams {
            project,
            query: "println!".to_string(),
            regex: None,
            case_sensitive: None,
            language: Some("rust".to_string()),
            file: None,
            limit: None,
            offset: None,
            before_context: Some(1),
            after_context: Some(1),
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(1));
        assert_eq!(result["matches"][0]["file"], json!("lib.rs"));
        assert_eq!(result["matches"][0]["line"], json!(2));
        assert_eq!(result["matches"][0]["column"], json!(5));
        assert_eq!(
            result["matches"][0]["before"][0]["text"],
            json!("fn hello_world() {")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_search_content_skips_file_symlink_escape() {
        use std::os::unix::fs::symlink;

        let project_dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret.rs");
        std::fs::write(&outside_file, "fn outside_secret() {}\n").unwrap();
        std::fs::write(project_dir.path().join("lib.rs"), "fn inside() {}\n").unwrap();
        symlink(&outside_file, project_dir.path().join("linked_secret.rs")).unwrap();
        let project = setup_indexed_project(&project_dir);

        let result = search_content(SearchContentParams {
            project,
            query: "outside_secret".to_string(),
            regex: None,
            case_sensitive: None,
            language: Some("rust".to_string()),
            file: None,
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(0));
    }

    #[tokio::test]
    async fn test_search_content_regex_and_file_filter() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("one.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(dir.path().join("two.rs"), "fn beta() {}\n").unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_content(SearchContentParams {
            project,
            query: r"fn\s+beta".to_string(),
            regex: Some(true),
            case_sensitive: Some(true),
            language: Some("rust".to_string()),
            file: Some("two.rs".to_string()),
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(1));
        assert_eq!(result["matches"][0]["file"], json!("two.rs"));
        assert_eq!(result["matches"][0]["line_text"], json!("fn beta() {}"));
        assert_eq!(
            result["guidance"]["next_step"],
            json!("Use the matched file paths to pivot to read_code_unit, locate_code, or trace_path instead of repeating nearby text searches.")
        );
        assert_eq!(
            result["steering"]["recommended_next_tool"],
            json!("read_code_unit"),
            "steering must only name tools that are exposed at every tier"
        );
    }

    #[tokio::test]
    async fn test_search_content_respects_env_excluded_dir_names() {
        let _guard = test_env_var_lock().lock().await;
        std::env::set_var(EXCLUDE_DIRS_ENV_VAR, "vendor");

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn keep_me() {}\n").unwrap();
        std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join("vendor/ignored.rs"), "fn find_me() {}\n").unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_content(SearchContentParams {
            project,
            query: "find_me".to_string(),
            regex: None,
            case_sensitive: None,
            language: Some("rust".to_string()),
            file: None,
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(0));

        std::env::remove_var(EXCLUDE_DIRS_ENV_VAR);
    }

    /// Tools that are exposed regardless of `PITLANE_MCP_TOOL_TIER`.
    const ALWAYS_EXPOSED: [&str; 14] = [
        "ensure_project_ready",
        "investigate",
        "locate_code",
        "read_code_unit",
        "trace_path",
        "analyze_impact",
        "analyze_changes",
        "get_index_changes",
        "get_index_stats",
        "doctor",
        "server_info",
        "search_content",
        "search_knowledge",
        "read_knowledge_document",
    ];

    async fn search(dir: &TempDir, query: &str, regex: Option<bool>) -> Value {
        let project = setup_indexed_project(dir);
        search_content(SearchContentParams {
            project,
            query: query.to_string(),
            regex,
            case_sensitive: None,
            language: Some("rust".to_string()),
            file: None,
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_zero_result_steering_escalates_to_another_exposed_tool() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn parse_flags() {}\n").unwrap();

        let result = search(&dir, "ParseCliArguments", None).await;

        assert_eq!(result["count"], json!(0));
        let recommended = result["steering"]["recommended_next_tool"]
            .as_str()
            .unwrap();
        assert_ne!(
            recommended, "search_content",
            "a zero-result search must not recommend repeating itself"
        );
        assert!(
            ALWAYS_EXPOSED.contains(&recommended),
            "{recommended} is not exposed at the default tier"
        );
        assert_eq!(
            result["steering"]["recommended_target"]["query"],
            json!("ParseCliArguments")
        );
        assert!(
            !result["guidance"]["next_step"]
                .as_str()
                .unwrap()
                .contains("search_symbols"),
            "guidance must not name a tool the host may not expose"
        );
    }

    #[tokio::test]
    async fn test_regex_syntax_searched_as_literal_is_reported() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn parse_flags() {}\n").unwrap();

        // Only an explicit `regex: false` can force the literal interpretation.
        let result = search(&dir, "flag.*parse|ParseFlags", Some(false)).await;

        assert_eq!(result["count"], json!(0));
        assert!(
            result["guidance"]["next_step"]
                .as_str()
                .unwrap()
                .contains("literal string"),
            "the caller must learn the pattern text was not interpreted"
        );
        assert!(result["steering"]["why_this_matched"]
            .as_str()
            .unwrap()
            .contains("regular expression"));
    }

    #[tokio::test]
    async fn test_regex_syntax_is_interpreted_when_flag_is_unset() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("parse.rs"),
            "use crate::flags::parse;\nfn parse_low() {}\n",
        )
        .unwrap();

        // The literal string `flags.*parse` cannot appear in source, so the old
        // behaviour guaranteed a zero result for this common guess.
        let auto = search(&dir, "flags.*parse", None).await;
        assert_eq!(auto["count"], json!(1));
        assert_eq!(auto["regex"], json!(true));
        assert_eq!(auto["matches"][0]["file"], json!("parse.rs"));

        // An explicit `regex: false` still means literal.
        let literal = search(&dir, "flags.*parse", Some(false)).await;
        assert_eq!(literal["count"], json!(0));
        assert_eq!(literal["regex"], json!(false));
    }

    #[tokio::test]
    async fn test_auto_detected_regex_degrades_instead_of_failing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn keep() {}\n").unwrap();

        // `.*` triggers auto-detection, but the pattern is invalid as a regex.
        let result = search(&dir, "keep.*(", None).await;
        assert_eq!(result["regex"], json!(false));
        assert_eq!(result["count"], json!(0));

        // An explicit regex request still reports the error.
        let project = setup_indexed_project(&dir);
        let error = search_content(SearchContentParams {
            project,
            query: "keep.*(".to_string(),
            regex: Some(true),
            case_sensitive: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await;
        assert!(
            error.is_err(),
            "an explicit regex must not be silently downgraded"
        );
    }

    #[tokio::test]
    async fn test_successful_match_recommends_an_always_exposed_tool() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn parse_flags() {\n    println!(\"parsed\");\n}\n",
        )
        .unwrap();

        let result = search(&dir, "println!", Some(true)).await;

        assert_eq!(result["count"], json!(1));
        let recommended = result["steering"]["recommended_next_tool"]
            .as_str()
            .unwrap();
        assert!(
            ALWAYS_EXPOSED.contains(&recommended),
            "{recommended} is not exposed at the default tier"
        );
        assert_eq!(
            result["steering"]["recommended_target"]["file_path"],
            json!("lib.rs")
        );
        assert_eq!(
            result["steering"]["recommended_target"]["line_start"],
            json!(2)
        );
        assert_eq!(
            result["steering"]["recommended_target"]["line_end"],
            json!(2)
        );
        assert_eq!(
            result["steering"]["recommended_target"]["project"],
            json!(dir.path().to_string_lossy().as_ref())
        );

        let target = &result["steering"]["recommended_target"];
        let read = read_code_unit(ReadCodeUnitParams {
            project: target["project"].as_str().unwrap().to_string(),
            symbol_id: None,
            file_path: Some(target["file_path"].as_str().unwrap().to_string()),
            line_start: Some(target["line_start"].as_u64().unwrap() as u32),
            line_end: Some(target["line_end"].as_u64().unwrap() as u32),
            include_context: None,
            signature_only: None,
        })
        .await
        .expect("recommended target must be accepted by read_code_unit");
        assert_eq!(read["source"], json!("    println!(\"parsed\");"));
    }

    #[tokio::test]
    async fn test_pipe_and_word_boundary_are_not_auto_regex() {
        let dir = TempDir::new().unwrap();
        // Several lines would match if `|` or `\b` silently enabled regex mode:
        // `||` and `|x| x + 1` match the empty string, `a|b` matches any line
        // containing `a` or `b`, and `C:\bin` treats `\b` as a word boundary
        // (so `C:index` matches and the path itself may not).
        std::fs::write(
            dir.path().join("lib.rs"),
            "\
fn alpha() {}
fn beta() {}
let closure = |x| x + 1;
let empty = ||;
let choice = \"a|b\";
let spaced = \"a | b\";
let assigned = \"b\";
let decoy = \"C:index\";
let path = \"C:\\bin\";
",
        )
        .unwrap();

        for query in ["||", "|x| x + 1", "a|b", "a | b", r"C:\bin"] {
            let result = search(&dir, query, None).await;
            assert_eq!(result["regex"], json!(false), "{query}");
            assert_eq!(result["count"], json!(1), "{query}");
            let line = result["matches"][0]["line_text"].as_str().unwrap();
            assert!(
                line.contains(query),
                "{query} should match literally, got {line}"
            );
            assert_ne!(
                result["matches"][0]["column"],
                json!(1),
                "{query} must not be an empty-string match at column 1"
            );
        }

        // Explicit regex mode still interprets these patterns.
        let forced = search(&dir, "||", Some(true)).await;
        assert_eq!(forced["regex"], json!(true));
        assert!(
            forced["count"].as_u64().unwrap() > 1,
            "explicit `||` still matches the empty string on every line"
        );
        assert!(forced["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["column"] == json!(1)));

        let boundary = search(&dir, r"C:\bin", Some(true)).await;
        assert_eq!(boundary["regex"], json!(true));
        assert_eq!(
            boundary["matches"][0]["line_text"],
            json!("let decoy = \"C:index\";")
        );
    }

    #[tokio::test]
    async fn test_auto_detected_regex_miss_is_reported_as_regex() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn parse_flags() {}\n").unwrap();

        let result = search(&dir, "zzz.*qqq", None).await;

        assert_eq!(result["count"], json!(0));
        assert_eq!(result["regex"], json!(true));
        let next_step = result["guidance"]["next_step"].as_str().unwrap();
        assert!(
            next_step.contains("regular expression"),
            "a regex miss must be described as a regex search: {next_step}"
        );
        assert!(
            !next_step.contains("literal"),
            "a regex miss must not be described as a literal search: {next_step}"
        );
        let why = result["steering"]["why_this_matched"].as_str().unwrap();
        assert!(why.contains("regular-expression"), "{why}");
        assert!(!why.contains("literal"), "{why}");
    }

    #[tokio::test]
    async fn test_auto_detected_regex_over_length_falls_back_to_literal() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn parse_flags() {}\n").unwrap();

        let query = format!("zzz.*{}", "q".repeat(MAX_REGEX_PATTERN_LEN));
        assert!(query.len() > MAX_REGEX_PATTERN_LEN);
        let result = search(&dir, &query, None).await;

        assert_eq!(result["regex"], json!(false));
        assert_eq!(result["count"], json!(0));
        let next_step = result["guidance"]["next_step"].as_str().unwrap();
        assert!(
            next_step.contains(&MAX_REGEX_PATTERN_LEN.to_string()),
            "{next_step}"
        );
        assert!(
            next_step.contains("literal"),
            "over-length auto-detection must say it fell back to literal search: {next_step}"
        );
        assert!(result["guidance"]["regex_fallback"]
            .as_str()
            .unwrap()
            .contains("literal"));

        let project = setup_indexed_project(&dir);
        let error = search_content(SearchContentParams {
            project,
            query,
            regex: Some(true),
            case_sensitive: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await;
        let message = format!(
            "{:#}",
            error.expect_err("explicit over-length regex must error")
        );
        assert!(
            message.contains(&MAX_REGEX_PATTERN_LEN.to_string()),
            "{message}"
        );
    }

    #[tokio::test]
    async fn test_auto_detected_empty_regex_falls_back_to_literal() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn one() {}\nfn two() {}\nfn three() {}\n",
        )
        .unwrap();

        // `.*` is an auto-detect marker, but it matches the empty string.
        let auto = search(&dir, ".*", None).await;
        assert_eq!(auto["regex"], json!(false));
        assert_eq!(auto["count"], json!(0));

        let forced = search(&dir, ".*", Some(true)).await;
        assert_eq!(forced["regex"], json!(true));
        assert_eq!(forced["count"], json!(3));
        assert!(forced["matches"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["column"] == json!(1)));
    }
}
