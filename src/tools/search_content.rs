use std::path::{Path, PathBuf};

use anyhow::Context;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use regex::RegexBuilder;
use serde_json::{json, Value};
use walkdir::{DirEntry, WalkDir};

use crate::error::ToolError;
use crate::indexer::{is_declaration_file, is_supported_extension, load_gitignore_patterns};
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

const DEFAULT_LIMIT: usize = 8;
const MAX_CONTEXT_LINES: usize = 5;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

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
    let regex = params.regex.unwrap_or(false);
    let case_sensitive = params.case_sensitive.unwrap_or(false);
    let before_context = params.before_context.unwrap_or(0).min(MAX_CONTEXT_LINES);
    let after_context = params.after_context.unwrap_or(0).min(MAX_CONTEXT_LINES);

    let file_glob = params
        .file
        .as_deref()
        .map(|f| {
            GlobBuilder::new(f)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()?;
    let language_filter = params
        .language
        .as_deref()
        .map(parse_language_filter)
        .transpose()?;
    let exclude_set = build_exclude_set(&canonical)?;

    let matcher = if regex {
        ContentMatcher::Regex(
            RegexBuilder::new(&params.query)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|e| ToolError::InvalidArgument {
                    param: "query".to_string(),
                    message: format!("invalid regex: {e}"),
                })?,
        )
    } else {
        ContentMatcher::Literal {
            needle: params.query.clone(),
            needle_folded: params.query.to_lowercase(),
            case_sensitive,
        }
    };

    let mut files = collect_searchable_files(
        &canonical,
        &exclude_set,
        file_glob.as_ref(),
        language_filter,
    )?;
    files.sort();

    let mut matches = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;

    'files: for path in files {
        let bytes = std::fs::read(&path)
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
    response["guidance"] = json!({
        "next_step": if matches.is_empty() {
            "If this text search did not find the right code, try a nearby literal/regex snippet or switch to search_symbols for behavior-based discovery."
        } else {
            "Use the matched file paths to pivot back to get_file_outline, search_symbols, or get_symbol instead of repeating nearby text searches."
        },
        "avoid": "Avoid shell grep when search_content can search the indexed source files directly."
    });
    let steering = if matches.is_empty() {
        build_steering(
            0.2,
            "No direct text match was found, so this is a weak content search result.".to_string(),
            "search_content",
            json!({ "query": params.query, "regex": regex }),
            take_fallback_candidates(&steering_matches),
        )
    } else {
        let top = &steering_matches[0];
        build_steering(
            0.86,
            "The matched line provides direct evidence for the requested text snippet.".to_string(),
            "get_file_outline",
            json!({
                "file": top["file"],
                "line": top["line"],
            }),
            take_fallback_candidates(&steering_matches),
        )
    };
    attach_steering(&mut response, steering);
    Ok(response)
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

fn parse_language_filter(language: &str) -> anyhow::Result<&'static [&'static str]> {
    match language.to_lowercase().as_str() {
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
                "Unknown language '{}'. Supported: rust, python, javascript, typescript, svelte, c, cpp, go, java, bash, csharp, ruby, swift, objc, php, zig, kotlin, lua, solidity",
                other
            ),
        }
        .into()),
    }
}

fn default_excludes() -> Vec<String> {
    vec![
        "target/**".to_string(),
        ".git/**".to_string(),
        "__pycache__/**".to_string(),
        "node_modules/**".to_string(),
        ".venv/**".to_string(),
        "venv/**".to_string(),
        "*.pyc".to_string(),
    ]
}

fn build_exclude_set(root: &Path) -> anyhow::Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in default_excludes()
        .into_iter()
        .chain(load_gitignore_patterns(root))
    {
        builder.add(globset::Glob::new(&pattern)?);
    }
    Ok(builder.build()?)
}

fn collect_searchable_files(
    root: &Path,
    exclude_set: &GlobSet,
    file_glob: Option<&globset::GlobMatcher>,
    language_filter: Option<&[&str]>,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(root, exclude_set, entry))
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
            continue;
        }
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if !is_supported_extension(ext) || is_declaration_file(path) {
            continue;
        }
        if let Some(exts) = language_filter {
            if !exts.contains(&ext) {
                continue;
            }
        }
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel.to_string_lossy();
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

fn should_descend(root: &Path, exclude_set: &GlobSet, entry: &DirEntry) -> bool {
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
    use crate::indexer::{registry, Indexer};
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
            json!("Use the matched file paths to pivot back to get_file_outline, search_symbols, or get_symbol instead of repeating nearby text searches.")
        );
    }
}
