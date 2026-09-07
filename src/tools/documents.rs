//! On-demand documentation/configuration discovery. These are line-addressed
//! units, not executable symbols, so they never create call-graph edges.
use std::path::Path;

use pulldown_cmark::{Event, Parser as MarkdownParser, Tag, TagEnd};
use serde_json::{json, Value};
use tree_sitter::Node;

use super::index_project::load_project_index;
use super::orchestrator::{truncate_chars, LocateCodeParams};
use super::search_content::{
    build_exclude_set, build_file_filter, collect_searchable_files, parse_language_filter,
};
use crate::index::SymbolIndex;
use crate::indexer::extra_excluded_dir_names;
use crate::path_policy::{read_regular_file, resolve_project_path};

pub(crate) fn is_document_extension(ext: &str) -> bool {
    matches!(ext, "md" | "markdown" | "json" | "yaml" | "yml" | "toml")
}

pub(crate) fn is_document_language(language: Option<&str>) -> bool {
    language.is_some_and(|lang| {
        matches!(
            lang.to_lowercase().as_str(),
            "markdown" | "md" | "json" | "yaml" | "yml" | "toml"
        )
    })
}

pub(crate) fn document_only(params: &LocateCodeParams) -> bool {
    if matches!(
        params.intent.as_deref(),
        Some("content" | "file" | "project" | "symbol")
    ) {
        return false;
    }
    matches!(
        params.intent.as_deref(),
        Some("documentation" | "docs" | "config")
    ) || matches!(params.kind.as_deref(), Some("section" | "config_key"))
        || is_document_language(params.language.as_deref())
}

#[derive(Debug)]
struct Unit {
    name: String,
    qualified: String,
    kind: &'static str,
    start: usize,
    end: usize,
    line_start: usize,
    line_end: usize,
}

// Offset lookup avoids rescanning the entire prefix for each heading/key.
fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn line_at(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|start| *start <= offset)
}

fn markdown_units(source: &str) -> Vec<Unit> {
    let starts = line_starts(source);
    let total_lines = source.lines().count().max(1);
    let mut units: Vec<Unit> = Vec::new();
    let mut levels: Vec<(usize, usize)> = Vec::new();
    let mut heading = None;
    for (event, range) in MarkdownParser::new(source).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let level = level as usize;
                while levels.last().is_some_and(|(old, _)| *old >= level) {
                    let (_, idx) = levels.pop().unwrap();
                    let next_line = line_at(&starts, range.start);
                    units[idx].end = starts[next_line - 1];
                    units[idx].line_end = next_line - 1;
                }
                let idx = units.len();
                units.push(Unit {
                    name: String::new(),
                    qualified: String::new(),
                    kind: "section",
                    start: range.start,
                    end: source.len(),
                    line_start: line_at(&starts, range.start),
                    line_end: total_lines,
                });
                levels.push((level, idx));
                heading = Some(idx);
            }
            Event::Text(text) | Event::Code(text) if heading.is_some() => {
                units[heading.unwrap()].name.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if heading.is_some() => {
                units[heading.unwrap()].name.push(' ');
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(idx) = heading.take() {
                    units[idx].qualified = levels
                        .iter()
                        .map(|(_, i)| units[*i].name.as_str())
                        .collect::<Vec<_>>()
                        .join(" > ");
                }
            }
            _ => {}
        }
    }
    units
}

fn key_text(node: Node<'_>, source: &str) -> String {
    if node.kind() == "dotted_key" {
        let mut parts = Vec::new();
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            if node.kind() == "dotted_key" {
                let mut cursor = node.walk();
                let children: Vec<_> = node.named_children(&mut cursor).collect();
                pending.extend(children.into_iter().rev());
            } else {
                parts.push(key_text(node, source));
            }
        }
        return parts.join(".");
    }
    let raw = &source[node.byte_range()];
    if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.trim_matches('"').to_string())
    } else if raw.starts_with('\'') && raw.ends_with('\'') {
        raw[1..raw.len() - 1].replace("''", "'")
    } else {
        raw.to_string()
    }
}

fn config_units(source: &str, ext: &str) -> anyhow::Result<Vec<Unit>> {
    let language = match ext {
        "json" => tree_sitter_json::LANGUAGE.into(),
        "yaml" | "yml" => tree_sitter_yaml::LANGUAGE.into(),
        "toml" => tree_sitter_toml_ng::LANGUAGE.into(),
        _ => return Ok(Vec::new()),
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language)?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(Vec::new());
    };
    let mut units = Vec::new();
    // Iterative traversal keeps deeply nested input off the Rust call stack.
    let mut pending = vec![(tree.root_node(), String::new())];
    while let Some((node, parent)) = pending.pop() {
        if node.is_error() || node.is_missing() {
            continue;
        }
        let table = ext == "toml" && matches!(node.kind(), "table" | "table_array_element");
        let pair = matches!(node.kind(), "pair" | "block_mapping_pair" | "flow_pair");
        let key = if ext == "toml" && (table || pair) {
            node.named_child(0)
        } else if pair {
            node.child_by_field_name("key")
        } else {
            None
        };
        if let Some(mut key) = key {
            if key.has_error() {
                continue;
            }
            let key_id = key.id();
            if matches!(ext, "yaml" | "yml") {
                let mut cursor = key.walk();
                let scalar = key.named_children(&mut cursor).find(|n| {
                    matches!(
                        n.kind(),
                        "plain_scalar" | "single_quote_scalar" | "double_quote_scalar"
                    )
                });
                let Some(scalar) = scalar else {
                    continue;
                };
                key = scalar;
            }
            let name = key_text(key, source);
            if name.contains('\n') {
                continue;
            }
            let qualified = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}.{name}")
            };
            let end = node.end_byte();
            units.push(Unit {
                name,
                qualified: qualified.clone(),
                kind: "config_key",
                start: node.start_byte(),
                end,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + usize::from(node.end_position().column > 0),
            });
            let mut cursor = node.walk();
            let children: Vec<_> = node
                .named_children(&mut cursor)
                .filter(|child| child.id() != key_id)
                .collect();
            for child in children.into_iter().rev() {
                pending.push((child, qualified.clone()));
            }
        } else {
            let mut cursor = node.walk();
            let children: Vec<_> = node.named_children(&mut cursor).collect();
            for child in children.into_iter().rev() {
                pending.push((child, parent.clone()));
            }
        }
    }
    Ok(units)
}

fn units(source: &str, ext: &str) -> anyhow::Result<Vec<Unit>> {
    if matches!(ext, "md" | "markdown") {
        Ok(markdown_units(source))
    } else {
        config_units(source, ext)
    }
}

fn match_score(unit: &Unit, query: &str) -> usize {
    let query = query.trim().trim_matches('`').to_lowercase();
    let name = unit.name.to_lowercase();
    let qualified = unit.qualified.to_lowercase();
    if name == query || qualified == query {
        return 100;
    }
    if name.contains(&query) {
        return 80;
    }
    let tokens: Vec<_> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| {
            !t.is_empty()
                && !matches!(
                    *t,
                    "the"
                        | "a"
                        | "an"
                        | "how"
                        | "where"
                        | "is"
                        | "are"
                        | "to"
                        | "of"
                        | "for"
                        | "in"
                )
        })
        .collect();
    if !tokens.is_empty() && tokens.iter().all(|t| qualified.contains(t)) {
        50
    } else {
        0
    }
}

fn as_result(unit: &Unit, file: &str, source: &str) -> Value {
    json!({
        "kind": unit.kind, "name": unit.name, "qualified": unit.qualified,
        "file": file, "line_start": unit.line_start, "line_end": unit.line_end,
        "signature": truncate_chars(source[unit.start..unit.end].lines().next().unwrap_or(""), 160),
        "read_target": {"file_path": file, "line_start": unit.line_start, "line_end": unit.line_end},
        "source_tool": "document_discovery",
    })
}

pub(crate) fn discover(params: &LocateCodeParams, limit: usize) -> anyhow::Result<Vec<Value>> {
    if params.language.as_deref().is_some_and(|lang| {
        parse_language_filter(lang)
            .is_ok_and(|exts| !exts.iter().any(|ext| is_document_extension(ext)))
    }) {
        return Ok(Vec::new());
    }
    let root = resolve_project_path(&params.project)?;
    let index = load_project_index(&params.project)?;
    let language = params
        .language
        .as_deref()
        .map(parse_language_filter)
        .transpose()?;
    let scope = params
        .scope
        .as_deref()
        .map(|scope| build_file_filter(&root, scope))
        .transpose()?;
    let excludes = build_exclude_set(&root)?;
    let mut files = collect_searchable_files(
        &root,
        &excludes,
        &extra_excluded_dir_names(),
        scope.as_ref(),
        None,
    )?;
    files.sort();
    let eligible_files: std::collections::HashSet<_> = files.iter().cloned().collect();
    let mut found: Vec<(usize, Value, String)> = Vec::new();
    for file in files {
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_document_extension(ext) || language.is_some_and(|exts| !exts.contains(&ext)) {
            continue;
        }
        let markdown = matches!(ext, "md" | "markdown");
        if (matches!(params.intent.as_deref(), Some("docs" | "documentation")) && !markdown)
            || (params.intent.as_deref() == Some("config") && markdown)
        {
            continue;
        }
        let bytes = read_regular_file(&file)?;
        let Ok(source) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let relative = file
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace('\\', "/");
        for unit in units(source, ext)? {
            if params.kind.as_deref().is_some_and(|kind| kind != unit.kind) {
                continue;
            }
            let score = match_score(&unit, &params.query);
            if score == 0 {
                continue;
            }
            found.push((
                score,
                as_result(&unit, &relative, source),
                source[unit.start..unit.end].to_string(),
            ));
            found.sort_by(|a, b| b.0.cmp(&a.0));
            found.truncate(limit);
        }
    }
    Ok(found
        .into_iter()
        .map(|(_, mut result, source)| {
            result["related_source"] =
                related_source(&index, &root, &result, &source, &eligible_files);
            result
        })
        .collect())
}

fn related_source(
    index: &SymbolIndex,
    root: &Path,
    result: &Value,
    source: &str,
    eligible_files: &std::collections::HashSet<std::path::PathBuf>,
) -> Value {
    // An exact spelling is lexical evidence, not a resolved usage or call.
    let mut symbols: Vec<_> = index
        .symbols
        .values()
        .filter(|sym| {
            eligible_files.contains(sym.file.as_path())
                && ((result["kind"] == "config_key"
                    && result["name"].as_str() == Some(sym.name.as_str()))
                    || (result["kind"] == "section" && source.contains(&format!("`{}`", sym.name))))
        })
        .collect();
    symbols.sort_by(|a, b| a.id.cmp(&b.id));
    json!(symbols.into_iter().take(3).map(|sym| json!({
        "symbol_id": sym.id, "name": sym.name,
        "file": sym.file.strip_prefix(root).unwrap_or(&sym.file),
        "line_start": sym.line_start,
        "evidence": if result["kind"] == "config_key" && result["name"].as_str() == Some(sym.name.as_str()) {
            "configuration key exactly matches symbol name"
        } else { "section contains the symbol name in backticks" },
        "relation": "lexical_reference", "resolved": false,
    })).collect::<Vec<_>>())
}

pub(crate) fn outline(source: &str, file: &str, limit: usize) -> anyhow::Result<Value> {
    let ext = Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let units = units(source, ext)?;
    let results: Vec<_> = units
        .iter()
        .take(limit)
        .map(|u| as_result(u, file, source))
        .collect();
    Ok(
        json!({"file": file, "count": units.len(), "returned_count": results.len(),
        "truncated": units.len() > limit, "units": results,
        "guidance": "Use read_code_unit with a unit's read_target to read its section or configuration value."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::{registry, Indexer};
    use crate::tools::orchestrator::{locate_code, read_code_unit, ReadCodeUnitParams};
    use crate::tools::search_content::{search_content, SearchContentParams};
    use tempfile::TempDir;

    fn project(files: &[(&str, &str)]) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        for (file, source) in files {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let canonical = dir.path().canonicalize().unwrap();
        let (index, _) = Indexer::new(registry::build_default_registry())
            .index_project(&canonical, &[])
            .unwrap();
        crate::cache::insert(canonical.clone(), index);
        (dir, canonical.to_string_lossy().into_owned())
    }

    fn params(project: &str, query: &str) -> LocateCodeParams {
        LocateCodeParams {
            project: project.into(),
            query: query.into(),
            intent: None,
            kind: None,
            language: None,
            scope: None,
            limit: Some(8),
        }
    }

    #[test]
    fn markdown_sections_keep_hierarchy_ranges_and_ignore_code() {
        let source = "# Setup\nIntro\n## **Retry** `policy`\nDetails\n```md\n# Fake\n```\n## Retry policy\n\nFinal\n=====\nEnd\n";
        let units = markdown_units(source);
        assert_eq!(
            units.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Setup", "Retry policy", "Retry policy", "Final"]
        );
        assert_eq!(units[1].qualified, "Setup > Retry policy");
        assert_eq!((units[0].line_start, units[0].line_end), (1, 9));
        assert_eq!((units[1].line_start, units[1].line_end), (3, 7));
        assert_eq!((units[2].line_start, units[2].line_end), (8, 9));
        assert_eq!((units[3].line_start, units[3].line_end), (10, 12));
    }

    #[test]
    fn config_parsers_find_nested_quoted_inline_and_array_keys() {
        for (ext, source) in [
            ("json", "{\"server\": {\"retry_count\": 3, \"peers\": [{\"host\": \"localhost\"}]}, \"message\": \"fake: key\"}"),
            ("yaml", "server:\n  'retry_count': 3\n  peers: [{host: localhost}]\nmessage: |\n  fake: key\n"),
            ("toml", "message = '''\nfake = 1\n'''\n[server]\n'retry_count' = 3\npeers = [{host = 'localhost'}]\n"),
        ] {
            let units = config_units(source, ext).unwrap();
            let names: Vec<_> = units.iter().map(|u| u.qualified.as_str()).collect();
            assert!(names.contains(&"server.retry_count"), "{ext}: {names:?}");
            assert!(names.contains(&"server.peers.host"), "{ext}: {names:?}");
            assert!(!names.iter().any(|n| n.contains("fake")), "{ext}: {names:?}");
            for unit in units {
                assert!(unit.line_start >= 1 && unit.line_end >= unit.line_start, "{ext}: {unit:?}");
                assert!(!source[unit.start..unit.end].is_empty());
            }
        }
    }

    #[test]
    fn toml_dotted_keys_tables_and_json_escaped_keys() {
        let toml = config_units("[server.http]\nretry.count = 3\n[[workers]]\nname = 'one'\n[[workers]]\nname = 'two'\n", "toml").unwrap();
        assert_eq!(
            toml.iter()
                .map(|u| u.qualified.as_str())
                .collect::<Vec<_>>(),
            [
                "server.http",
                "server.http.retry.count",
                "workers",
                "workers.name",
                "workers",
                "workers.name"
            ]
        );
        let json = config_units(r#"{"caf\u00e9": 1}"#, "json").unwrap();
        assert_eq!(json[0].name, "café");
        let quoted = config_units("[\"server\".\"http\"]\n'custom.key' = 1\n", "toml").unwrap();
        assert_eq!(quoted[1].qualified, "server.http.custom.key");
        let literal = config_units(r#"{"?key": 1, "[key]": 2}"#, "json").unwrap();
        assert_eq!(literal.len(), 2);
        let yaml = config_units("'[key]': 1\n? [complex, key]\n: 2\n", "yaml").unwrap();
        assert_eq!(yaml.len(), 1);
        assert_eq!(yaml[0].name, "[key]");
        let indented = markdown_units("# First\n  ## Child\nContent\n  # Next\n");
        assert_eq!(indented[0].line_end, 3);
        assert_eq!(indented[1].line_end, 3);
    }

    #[tokio::test]
    async fn locate_documents_and_read_ranges_with_freshness() {
        let (dir, project) = project(&[
            (
                "README.md",
                "# Retry policy\nUse `retry_count`.\n# Other\nOther details\n",
            ),
            ("lib.rs", "const retry_count: usize = 3;\n"),
        ]);
        let mut p = params(&project, "Retry policy");
        p.intent = Some("docs".into());
        let response = locate_code(p).await.unwrap();
        let hit = &response["results"][0];
        assert_eq!(hit["kind"], "section");
        assert_eq!(hit["related_source"][0]["name"], "retry_count");
        assert_eq!(hit["related_source"][0]["resolved"], false);
        assert!(hit["id"].is_null());
        assert_eq!(
            hit["read_target"],
            json!({"file_path":"README.md", "line_start":1, "line_end":2})
        );
        let read = || ReadCodeUnitParams {
            project: project.clone(),
            file_path: Some("README.md".into()),
            symbol_id: None,
            line_start: Some(1),
            line_end: Some(2),
            include_context: None,
            signature_only: None,
        };
        let first = read_code_unit(read()).await.unwrap();
        assert_eq!(first["source"], "# Retry policy\nUse `retry_count`.");
        assert_eq!(
            read_code_unit(read()).await.unwrap()["read_state"]["status"],
            "unchanged"
        );
        std::fs::write(dir.path().join("README.md"), "# Retry policy\nUpdated.\n").unwrap();
        assert_eq!(
            read_code_unit(read()).await.unwrap()["read_state"]["status"],
            "changed"
        );
        let mut outline_read = read();
        outline_read.line_start = None;
        outline_read.line_end = None;
        let outline = read_code_unit(outline_read).await.unwrap();
        assert_eq!(outline["count"], 1);
        assert_eq!(outline["units"][0]["name"], "Retry policy");
    }

    #[tokio::test]
    async fn ambiguous_lookup_includes_code_and_config() {
        let (_dir, project) = project(&[
            ("lib.rs", "fn retry_count() {}\n"),
            ("settings.json", "{\"retry_count\": 3}"),
        ]);
        let response = locate_code(params(&project, "retry_count")).await.unwrap();
        let hits = response["results"].as_array().unwrap();
        assert!(hits.iter().any(|h| h["kind"] == "symbol"), "{response}");
        assert!(hits.iter().any(|h| h["kind"] == "config_key"), "{response}");
        let mut p = params(&project, "retry_count");
        p.language = Some("json".into());
        let response = locate_code(p).await.unwrap();
        assert_eq!(response["count"], 1);
        assert_eq!(response["results"][0]["kind"], "config_key");
        let mut p = params(&project, "retry_count");
        p.intent = Some("symbol".into());
        let response = locate_code(p).await.unwrap();
        assert!(response["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["kind"] == "symbol"));
    }

    #[tokio::test]
    async fn scope_exclusions_and_current_document_files_are_respected() {
        let (dir, project) = project(&[
            ("docs/setup.md", "# Retry\n"),
            ("other.md", "# Retry\n"),
            ("node_modules/a.md", "# Retry\n"),
            ("hidden/a.yaml", "retry: 3\n"),
            (".gitignore", "hidden/\n"),
            ("lib.rs", "fn retry() {}\n"),
        ]);
        let mut p = params(&project, "Retry");
        p.intent = Some("docs".into());
        p.scope = Some("docs".into());
        let hits = discover(&p, 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["file"], "docs/setup.md");
        assert_eq!(hits[0]["related_source"], json!([]));
        p.scope = None;
        p.intent = None;
        assert_eq!(discover(&p, 8).unwrap().len(), 2);
        std::fs::write(dir.path().join("new.toml"), "retry = 9\n").unwrap();
        assert_eq!(discover(&p, 8).unwrap().len(), 3);
        std::fs::write(dir.path().join("new.toml"), "other = 9\n").unwrap();
        assert_eq!(discover(&p, 8).unwrap().len(), 2);
        assert_eq!(discover(&p, 1).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn document_language_preserves_explicit_content_and_file_routes() {
        let (_dir, project) = project(&[(
            "config.json",
            "{\"retry_count\": 3, \"value\": \"sentinel\"}",
        )]);
        let mut p = params(&project, "sentinel");
        p.intent = Some("content".into());
        p.language = Some("json".into());
        let response = locate_code(p).await.unwrap();
        assert_eq!(response["results"][0]["kind"], "content");
        assert_eq!(
            response["results"][0]["read_target"]["file_path"],
            "config.json"
        );
        let mut p = params(&project, "notpresentanywhere");
        p.intent = Some("content".into());
        p.language = Some("json".into());
        assert_eq!(locate_code(p).await.unwrap()["count"], 0);
        let mut p = params(&project, "config.json");
        p.intent = Some("file".into());
        p.language = Some("json".into());
        assert_eq!(locate_code(p).await.unwrap()["count"], 1);
    }

    #[tokio::test]
    async fn saved_custom_exclusions_apply_to_docs_and_source_text() {
        let (dir, project) = project(&[
            ("skip.md", "# Retry\n"),
            ("skip.rs", "fn retry() {}"),
            ("keep.yaml", "retry: 3"),
        ]);
        let root = Path::new(&project);
        let mut meta = crate::index::format::IndexMeta::new(root);
        meta.effective_excludes = vec!["skip.*".into()];
        let index_dir = crate::index::format::index_dir(root).unwrap();
        std::fs::create_dir_all(&index_dir).unwrap();
        crate::index::format::save_meta(&meta, &index_dir.join("meta.json")).unwrap();
        let hits = discover(&params(&project, "retry"), 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["file"], "keep.yaml");
        let result = search_content(SearchContentParams {
            project: project.clone(),
            query: "retry".into(),
            regex: None,
            case_sensitive: None,
            language: None,
            file: None,
            limit: Some(10),
            offset: None,
            before_context: None,
            after_context: None,
        })
        .await
        .unwrap();
        assert_eq!(result["count"], 1);
        assert_eq!(result["matches"][0]["file"], "keep.yaml");
        drop(dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_oversized_files_and_non_utf8_documents_are_skipped() {
        let (dir, project) = project(&[("keep.md", "# Retry")]);
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("outside.md"), "# Retry").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("outside.md"),
            dir.path().join("link.md"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("large.md"),
            format!("# Retry\n{}", "x".repeat(1024 * 1024)),
        )
        .unwrap();
        std::fs::write(dir.path().join("binary.md"), [0xff, 0xfe]).unwrap();
        assert_eq!(discover(&params(&project, "retry"), 8).unwrap().len(), 1);
    }
}
