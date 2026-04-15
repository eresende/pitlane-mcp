use std::cmp::Ordering;
use std::path::Path;

use anyhow::Context;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};
use walkdir::{DirEntry, WalkDir};

use crate::error::ToolError;
use crate::indexer::load_gitignore_patterns;
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;
use crate::tools::steering::{attach_steering, build_steering, take_fallback_candidates};

const DEFAULT_LIMIT: usize = 8;

pub struct SearchFilesParams {
    pub project: String,
    pub query: String,
    pub mode: Option<String>,
    pub language: Option<String>,
    pub file: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn search_files(params: SearchFilesParams) -> anyhow::Result<Value> {
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
    let mode = params.mode.as_deref().unwrap_or("substring");
    let language_filter = params
        .language
        .as_deref()
        .map(parse_language_filter)
        .transpose()?;
    let scope_glob = params
        .file
        .as_deref()
        .map(|f| {
            GlobBuilder::new(f)
                .case_insensitive(true)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()?;
    let exclude_set = build_exclude_set(&canonical)?;

    let matcher = FileMatcher::new(mode, &params.query)?;

    let mut matches: Vec<FileMatch> = collect_files(
        &canonical,
        &exclude_set,
        scope_glob.as_ref(),
        language_filter,
        &matcher,
    )?;

    matches.sort_by(compare_matches);

    let total = matches.len();
    let page: Vec<FileMatch> = matches.into_iter().skip(offset).take(limit).collect();
    let truncated = offset + page.len() < total;
    let steering_page = page
        .iter()
        .map(|m| {
            json!({
                "file": m.file,
                "file_name": m.file_name,
                "extension": m.extension,
                "match_type": m.match_type,
                "score": m.score,
            })
        })
        .collect::<Vec<_>>();

    let mut response = json!({
        "results": steering_page,
        "count": page.len(),
        "query": params.query,
        "mode": mode,
        "truncated": truncated,
    });
    if truncated {
        response["next_page_message"] = json!(format!(
            "More files available. Call again with offset: {}",
            offset + limit
        ));
    }
    response["guidance"] = json!({
        "next_step": if page.is_empty() {
            "If this did not find the right file, try a more specific path fragment, switch modes, or use search_symbols/search_content when you know a symbol or text snippet."
        } else {
            "Use the returned file paths with get_file_outline, search_content, or get_symbol instead of switching to shell globbing."
        },
        "avoid": "Avoid shell globbing or find when search_files can locate repository paths directly."
    });
    let steering = if page.is_empty() {
        build_steering(
            0.22,
            "No strong file match was found, so this is a weak repository-path discovery result.",
            "search_files",
            json!({ "query": params.query, "mode": mode }),
            take_fallback_candidates(&response["results"].as_array().cloned().unwrap_or_default()),
        )
    } else {
        let top = &page[0];
        build_steering(
            match mode {
                "exact" => 0.99,
                "substring" => 0.88,
                "fuzzy" => 0.74,
                "glob" => 0.92,
                _ => 0.8,
            },
            "The top file result matches the requested path intent.".to_string(),
            "get_file_outline",
            json!({
                "file": top.file,
                "match_type": top.match_type,
            }),
            take_fallback_candidates(&response["results"].as_array().cloned().unwrap_or_default()),
        )
    };
    attach_steering(&mut response, steering);
    Ok(response)
}

#[derive(Clone)]
struct FileMatch {
    file: String,
    file_name: String,
    extension: String,
    match_type: String,
    score: f32,
}

enum FileMatcher {
    Exact { query: String },
    Substring { query: String },
    Fuzzy { query: String },
    Glob(globset::GlobMatcher),
}

impl FileMatcher {
    fn new(mode: &str, query: &str) -> anyhow::Result<Self> {
        let folded = query.trim().to_lowercase();
        match mode {
            "exact" => Ok(Self::Exact { query: folded }),
            "substring" => Ok(Self::Substring { query: folded }),
            "fuzzy" => Ok(Self::Fuzzy { query: folded }),
            "glob" => Ok(Self::Glob(
                GlobBuilder::new(query)
                    .case_insensitive(true)
                    .build()
                    .map_err(|e| ToolError::InvalidArgument {
                        param: "query".to_string(),
                        message: format!("invalid glob: {e}"),
                    })?
                    .compile_matcher(),
            )),
            other => Err(ToolError::InvalidArgument {
                param: "mode".to_string(),
                message: format!(
                    "Unknown mode '{}'. Supported: substring, exact, fuzzy, glob",
                    other
                ),
            }
            .into()),
        }
    }

    fn matches(&self, rel_path: &str, file_name: &str) -> Option<(f32, &'static str)> {
        let rel_folded = rel_path.to_lowercase();
        let name_folded = file_name.to_lowercase();
        match self {
            Self::Exact { query } => {
                if &name_folded == query {
                    Some((300.0, "exact_name"))
                } else if &rel_folded == query {
                    Some((280.0, "exact_path"))
                } else {
                    None
                }
            }
            Self::Substring { query } => {
                if let Some(pos) = name_folded.find(query) {
                    Some((
                        220.0 - pos as f32 - file_name.len() as f32 * 0.01,
                        "name_substring",
                    ))
                } else if let Some(pos) = rel_folded.find(query) {
                    Some((
                        180.0 - pos as f32 - rel_path.len() as f32 * 0.01,
                        "path_substring",
                    ))
                } else {
                    None
                }
            }
            Self::Fuzzy { query } => {
                let name_score = trigram_similarity(query, &name_folded);
                let path_score = trigram_similarity(query, &rel_folded);
                let (score, match_type) = if name_score >= path_score {
                    (name_score, "name_fuzzy")
                } else {
                    (path_score, "path_fuzzy")
                };
                (score >= 0.2).then_some((score * 100.0, match_type))
            }
            Self::Glob(matcher) => {
                let rel_path_ref: &Path = rel_path.as_ref();
                matcher.is_match(rel_path_ref).then_some((200.0, "glob"))
            }
        }
    }
}

fn compare_matches(a: &FileMatch, b: &FileMatch) -> Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.file_name.cmp(&b.file_name))
        .then_with(|| a.file.cmp(&b.file))
}

fn collect_files(
    root: &Path,
    exclude_set: &GlobSet,
    scope_glob: Option<&globset::GlobMatcher>,
    language_filter: Option<&[&str]>,
    matcher: &FileMatcher,
) -> anyhow::Result<Vec<FileMatch>> {
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
        let rel = path.strip_prefix(root).unwrap_or(path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if exclude_set.is_match(rel_str.as_str()) || exclude_set.is_match(path) {
            continue;
        }
        if let Some(scope) = scope_glob {
            let rel_path = Path::new(&rel_str);
            if !scope.is_match(rel_path) {
                continue;
            }
        }

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("Failed to get file name for {}", path.display()))?;
        let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if let Some(exts) = language_filter {
            if !exts.contains(&extension) {
                continue;
            }
        }
        let Some((score, match_type)) = matcher.matches(&rel_str, file_name) else {
            continue;
        };

        files.push(FileMatch {
            file: rel_str,
            file_name: file_name.to_string(),
            extension: extension.to_string(),
            match_type: match_type.to_string(),
            score,
        });
    }
    Ok(files)
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

fn trigrams(s: &str) -> std::collections::HashSet<[char; 3]> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return std::collections::HashSet::new();
    }
    chars.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
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
    async fn test_search_files_substring_finds_tests() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            dir.path().join("tests/ImmutableListTest.java"),
            "class X {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("tests/ImmutableMapTest.java"),
            "class Y {}\n",
        )
        .unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_files(SearchFilesParams {
            project,
            query: "ImmutableList".to_string(),
            mode: None,
            language: None,
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(1));
        assert_eq!(
            result["results"][0]["file"],
            json!("tests/ImmutableListTest.java")
        );
        assert_eq!(result["results"][0]["match_type"], json!("name_substring"));
    }

    #[tokio::test]
    async fn test_search_files_glob_with_language_filter() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(
            dir.path()
                .join("guava-tests/test/com/google/common/collect"),
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("guava-tests/test/com/google/common/collect/ImmutableListTest.java"),
            "class T {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path()
                .join("guava-tests/test/com/google/common/collect/ImmutableListTest.kt"),
            "class T\n",
        )
        .unwrap();
        let project = setup_indexed_project(&dir);

        let result = search_files(SearchFilesParams {
            project,
            query: "**/ImmutableList*.java".to_string(),
            mode: Some("glob".to_string()),
            language: Some("java".to_string()),
            file: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(result["count"], json!(1));
        assert_eq!(
            result["results"][0]["file"],
            json!("guava-tests/test/com/google/common/collect/ImmutableListTest.java")
        );
        assert_eq!(
            result["guidance"]["next_step"],
            json!("Use the returned file paths with get_file_outline, search_content, or get_symbol instead of switching to shell globbing.")
        );
    }
}
