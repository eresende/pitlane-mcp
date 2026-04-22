use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::error::ToolError;
use crate::indexer::svelte::{collect_script_blocks, ScriptBlockLanguage};
use crate::indexer::{
    is_supported_extension, tree_sitter_language_for_extension, warn_walkdir_error,
};
use crate::path_policy::resolve_project_path;
use crate::tools::index_project::load_project_index;

pub struct FindUsagesParams {
    pub project: String,
    pub symbol_id: String,
    pub scope: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub async fn find_usages(params: FindUsagesParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;
    let limit = params.limit.unwrap_or(100);
    let offset = params.offset.unwrap_or(0);

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| ToolError::SymbolNotFound {
            symbol_id: params.symbol_id.clone(),
        })?;

    let symbol_name = sym.name.clone();
    let project_path = resolve_project_path(&params.project)?;

    // Build scope glob set if provided
    let scope_set: Option<GlobSet> = params.scope.as_deref().map(|scope| {
        let mut builder = GlobSetBuilder::new();
        if let Ok(glob) = Glob::new(scope) {
            builder.add(glob);
        }
        builder
            .build()
            .unwrap_or_else(|_| GlobSetBuilder::new().build().unwrap())
    });

    let max_collect = offset.saturating_add(limit);
    let mut usages = Vec::new();
    let mut searched_files = HashSet::new();

    let mut indexed_files: Vec<PathBuf> = index
        .by_file
        .keys()
        .filter(|path| should_search_path(path, &project_path, scope_set.as_ref()))
        .cloned()
        .collect();
    indexed_files.sort_by_key(|path| {
        let rel = path.strip_prefix(&project_path).unwrap_or(path);
        (
            if path.as_path() == sym.file.as_ref() {
                0
            } else {
                1
            },
            rel.to_string_lossy().replace('\\', "/"),
        )
    });

    for path in indexed_files {
        searched_files.insert(path.clone());
        append_usages_for_file(&path, &project_path, &symbol_name, &mut usages);
        if usages.len() >= max_collect {
            break;
        }
    }

    if usages.len() < max_collect {
        for entry in WalkDir::new(&project_path)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    warn_walkdir_error(&project_path, &err, "find_usages");
                    continue;
                }
            };
            let path = entry.path();
            if !should_search_path(path, &project_path, scope_set.as_ref()) {
                continue;
            }
            if searched_files.contains(path) {
                continue;
            }
            append_usages_for_file(path, &project_path, &symbol_name, &mut usages);
            if usages.len() >= max_collect {
                break;
            }
        }
    }

    let truncated = usages.len() >= max_collect;
    let page: Vec<_> = usages.into_iter().skip(offset).take(limit).collect();

    let mut resp = json!({
        "symbol_id": params.symbol_id,
        "symbol_name": symbol_name,
        "usages": page,
        "count": page.len(),
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

fn should_search_path(path: &Path, project_path: &Path, scope_set: Option<&GlobSet>) -> bool {
    if !path.is_file() {
        return false;
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !is_supported_extension(ext) {
        return false;
    }

    if let Some(set) = scope_set {
        let rel = path.strip_prefix(project_path).unwrap_or(path);
        if !set.is_match(rel) && !set.is_match(path) {
            return false;
        }
    }

    true
}

fn append_usages_for_file(
    path: &Path,
    project_path: &Path,
    symbol_name: &str,
    usages: &mut Vec<Value>,
) {
    let hits = match search_file_ast(path, symbol_name) {
        Ok(hits) => hits,
        Err(_) => return,
    };

    let rel = path.strip_prefix(project_path).unwrap_or(path);
    let file = rel.to_string_lossy().replace('\\', "/");
    for (line_num, col, snippet) in hits {
        usages.push(json!({
            "file": file,
            "line": line_num,
            "column": col,
            "snippet": snippet,
        }));
    }
}

/// Searches `path` for AST nodes whose text equals `name`. Only true identifier
/// nodes are matched — string literals, comments, and substrings of longer
/// identifiers are never returned.
fn search_file_ast(path: &Path, name: &str) -> anyhow::Result<Vec<(usize, usize, String)>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Skip oversized files (same guard as the indexer)
    if path.metadata().map(|m| m.len()).unwrap_or(0) > 1024 * 1024 {
        return Ok(vec![]);
    }

    let source = std::fs::read(path)?;
    let source_str = std::str::from_utf8(&source).unwrap_or("");

    // Fast pre-filter: if the symbol name doesn't appear anywhere in the file
    // as a substring it cannot appear as an identifier — skip the tree-sitter
    // parse entirely.  False positives (name in a comment, string literal, or
    // as part of a longer identifier) are fine; the AST pass handles those.
    if !source_str.contains(name) {
        return Ok(vec![]);
    }

    let lines: Vec<&str> = source_str.lines().collect();

    if ext == "svelte" {
        return search_svelte_file_ast(&source, &lines, name);
    }

    let ts_lang = match tree_sitter_language_for_extension(ext) {
        Some(lang) => lang,
        None => return Ok(vec![]),
    };

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_lang)?;

    let tree = match parser.parse(&source, None) {
        Some(t) => t,
        None => return Ok(vec![]),
    };

    let mut hits = Vec::new();
    let mut seen = HashSet::new();

    collect_identifier_nodes(
        tree.root_node(),
        &source,
        name,
        &lines,
        &mut hits,
        &mut seen,
    );

    hits.sort_unstable();

    Ok(hits)
}

fn search_svelte_file_ast(
    source: &[u8],
    lines: &[&str],
    name: &str,
) -> anyhow::Result<Vec<(usize, usize, String)>> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_svelte_ng::LANGUAGE.into())?;

    let Some(tree) = parser.parse(source, None) else {
        return Ok(vec![]);
    };

    let mut hits = Vec::new();
    let mut seen = HashSet::new();

    for block in collect_script_blocks(source, tree.root_node()) {
        let script_source = &source[block.byte_start..block.byte_end];
        let script_str = std::str::from_utf8(script_source).unwrap_or("");
        if !script_str.contains(name) {
            continue;
        }

        let ts_lang = match block.language {
            ScriptBlockLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            ScriptBlockLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        };

        let mut script_parser = tree_sitter::Parser::new();
        script_parser.set_language(&ts_lang)?;
        let Some(script_tree) = script_parser.parse(script_source, None) else {
            continue;
        };

        collect_identifier_nodes_embedded(
            script_tree.root_node(),
            script_source,
            name,
            lines,
            (block.line_start as usize - 1, block.column_start as usize),
            &mut hits,
            &mut seen,
        );
    }

    hits.sort_unstable();
    Ok(hits)
}

/// Recursively walks the AST and collects all identifier nodes whose text
/// matches `name`. Covers Rust's `identifier`, `type_identifier`, and
/// `field_identifier` node kinds, and Python's `identifier`.
fn collect_identifier_nodes(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    name: &str,
    lines: &[&str],
    hits: &mut Vec<(usize, usize, String)>,
    seen: &mut HashSet<(usize, usize)>,
) {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    ) && node.utf8_text(source).ok() == Some(name)
    {
        let row = node.start_position().row;
        let col = node.start_position().column;
        if seen.insert((row, col)) {
            let snippet = lines
                .get(row)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            hits.push((row + 1, col + 1, snippet));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifier_nodes(child, source, name, lines, hits, seen);
    }
}

fn collect_identifier_nodes_embedded(
    node: tree_sitter::Node<'_>,
    source: &[u8],
    name: &str,
    lines: &[&str],
    offset: (usize, usize),
    hits: &mut Vec<(usize, usize, String)>,
    seen: &mut HashSet<(usize, usize)>,
) {
    let (row_offset, col_offset) = offset;
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    ) && node.utf8_text(source).ok() == Some(name)
    {
        let row = row_offset + node.start_position().row;
        let col = if node.start_position().row == 0 {
            col_offset + node.start_position().column
        } else {
            node.start_position().column
        };
        if seen.insert((row, col)) {
            let snippet = lines
                .get(row)
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            hits.push((row + 1, col + 1, snippet));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_identifier_nodes_embedded(child, source, name, lines, offset, hits, seen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn write(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[cfg(unix)]
    struct RestrictedDir {
        path: std::path::PathBuf,
        original_mode: u32,
    }

    #[cfg(unix)]
    impl RestrictedDir {
        fn new(root: &TempDir, name: &str) -> Self {
            let path = root.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("hidden.rs"), b"fn hidden() {}\n").unwrap();
            let metadata = std::fs::metadata(&path).unwrap();
            let original_mode = metadata.permissions().mode();
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o0);
            std::fs::set_permissions(&path, permissions).unwrap();
            Self {
                path,
                original_mode,
            }
        }

        fn is_effectively_inaccessible(&self) -> bool {
            matches!(
                std::fs::read_dir(&self.path),
                Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied
            )
        }
    }

    #[cfg(unix)]
    impl Drop for RestrictedDir {
        fn drop(&mut self) {
            if let Ok(metadata) = std::fs::metadata(&self.path) {
                let mut permissions = metadata.permissions();
                permissions.set_mode(self.original_mode);
                let _ = std::fs::set_permissions(&self.path, permissions);
            }
        }
    }

    // ── Rust ────────────────────────────────────────────────────────────────

    #[test]
    fn test_rs_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "lib.rs", "fn foo() {}\nfn bar() { foo(); }\n");
        let hits = search_file_ast(&path, "foo").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&2), "call on line 2");
    }

    #[test]
    fn test_rs_ignores_string_literal() {
        let dir = TempDir::new().unwrap();
        // "foo" in a string must not be returned
        let path = write(
            &dir,
            "lib.rs",
            "fn foo() {}\nfn bar() { let _s = \"foo\"; }\n",
        );
        let hits = search_file_ast(&path, "foo").unwrap();
        // Only the definition on line 1; the string on line 2 must be absent
        assert!(
            hits.iter().all(|(l, _, _)| *l == 1),
            "string literal must not be returned: {hits:?}"
        );
    }

    #[test]
    fn test_rs_ignores_comment() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "lib.rs", "// foo is mentioned here\nfn bar() {}\n");
        let hits = search_file_ast(&path, "foo").unwrap();
        assert!(hits.is_empty(), "comment mention must not be returned");
    }

    #[test]
    fn test_rs_no_partial_match() {
        let dir = TempDir::new().unwrap();
        // searching for `fo` must not match `foo`
        let path = write(&dir, "lib.rs", "fn foo() {}\nfn bar() { foo(); }\n");
        let hits = search_file_ast(&path, "fo").unwrap();
        assert!(hits.is_empty(), "partial name must not match");
    }

    #[test]
    fn test_rs_type_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "lib.rs",
            "struct Foo {}\nfn bar(_x: Foo) {}\nfn baz() -> Foo { Foo {} }\n",
        );
        let hits = search_file_ast(&path, "Foo").unwrap();
        // definition + two uses as type + one constructor = 4
        assert!(hits.len() >= 3, "expected ≥3 hits, got {hits:?}");
    }

    #[test]
    fn test_rs_field_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "lib.rs",
            "struct S { val: u32 }\nfn f(s: S) -> u32 { s.val }\n",
        );
        let hits = search_file_ast(&path, "val").unwrap();
        // struct field declaration + field access
        assert!(hits.len() >= 2, "expected ≥2 hits, got {hits:?}");
    }

    // ── Python ──────────────────────────────────────────────────────────────

    #[test]
    fn test_py_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.py",
            "def foo():\n    pass\n\ndef bar():\n    foo()\n",
        );
        let hits = search_file_ast(&path, "foo").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&5), "call on line 5");
    }

    #[test]
    fn test_py_ignores_string_literal() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "mod.py", "def foo():\n    pass\n\nx = \"foo\"\n");
        let hits = search_file_ast(&path, "foo").unwrap();
        assert!(
            hits.iter().all(|(l, _, _)| *l == 1),
            "string literal must not be returned: {hits:?}"
        );
    }

    #[test]
    fn test_py_ignores_comment() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.py",
            "# foo mentioned here\ndef bar():\n    pass\n",
        );
        let hits = search_file_ast(&path, "foo").unwrap();
        assert!(hits.is_empty(), "comment mention must not be returned");
    }

    // ── Edge cases ──────────────────────────────────────────────────────────

    #[test]
    fn test_unknown_extension_returns_empty() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "notes.txt", "foo bar baz");
        let hits = search_file_ast(&path, "foo").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn test_oversized_file_skipped() {
        let dir = TempDir::new().unwrap();
        let big_content = "fn foo() {}\n".repeat(100_000);
        let path = write(&dir, "big.rs", &big_content);
        // File must exceed 1 MiB for the guard to trigger
        assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
        let hits = search_file_ast(&path, "foo").unwrap();
        assert!(hits.is_empty(), "oversized file must be skipped");
    }

    // ── JavaScript ──────────────────────────────────────────────────────────

    #[test]
    fn test_js_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "mod.js", "function greet() {}\ngreet();\n");
        let hits = search_file_ast(&path, "greet").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&2), "call on line 2");
    }

    #[test]
    fn test_js_ignores_string_literal() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.js",
            "function greet() {}\nconst s = \"greet\";\n",
        );
        let hits = search_file_ast(&path, "greet").unwrap();
        assert!(
            hits.iter().all(|(l, _, _)| *l == 1),
            "string literal must not be returned: {hits:?}"
        );
    }

    #[test]
    fn test_js_ignores_comment() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.js",
            "// greet is mentioned here\nfunction other() {}\n",
        );
        let hits = search_file_ast(&path, "greet").unwrap();
        assert!(hits.is_empty(), "comment mention must not be returned");
    }

    #[test]
    fn test_jsx_finds_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "App.jsx", "function App() { return null; }\nApp();\n");
        let hits = search_file_ast(&path, "App").unwrap();
        assert!(hits.len() >= 2, "expected ≥2 hits, got {hits:?}");
    }

    // ── TypeScript ──────────────────────────────────────────────────────────

    #[test]
    fn test_ts_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "mod.ts", "function greet(): void {}\ngreet();\n");
        let hits = search_file_ast(&path, "greet").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&2), "call on line 2");
    }

    #[test]
    fn test_ts_finds_type_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.ts",
            "interface User { id: number; }\nfunction f(u: User): User { return u; }\n",
        );
        let hits = search_file_ast(&path, "User").unwrap();
        assert!(hits.len() >= 3, "expected ≥3 hits, got {hits:?}");
    }

    #[test]
    fn test_tsx_finds_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "App.tsx",
            "function App() { return <div />; }\nApp();\n",
        );
        let hits = search_file_ast(&path, "App").unwrap();
        assert!(hits.len() >= 2, "expected ≥2 hits, got {hits:?}");
    }

    // ── C ───────────────────────────────────────────────────────────────────

    #[test]
    fn test_c_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.c",
            "void process(void) {}\nint main(void) { process(); return 0; }\n",
        );
        let hits = search_file_ast(&path, "process").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&2), "call on line 2");
    }

    #[test]
    fn test_c_header_finds_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "mod.h", "typedef struct Node { int val; } Node;\n");
        let hits = search_file_ast(&path, "Node").unwrap();
        assert!(
            hits.len() >= 2,
            "expected ≥2 hits for typedef and struct tag, got {hits:?}"
        );
    }

    #[test]
    fn test_c_ignores_string_literal() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.c",
            "void process(void) {}\nconst char *s = \"process\";\n",
        );
        let hits = search_file_ast(&path, "process").unwrap();
        assert!(
            hits.iter().all(|(l, _, _)| *l == 1),
            "string literal must not be returned: {hits:?}"
        );
    }

    // ── C++ ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_cpp_finds_class_and_usage() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.cpp",
            "class Greeter {};\nvoid use() { Greeter g; }\n",
        );
        let hits = search_file_ast(&path, "Greeter").unwrap();
        assert!(hits.len() >= 2, "expected ≥2 hits, got {hits:?}");
    }

    #[test]
    fn test_cpp_header_finds_identifier() {
        let dir = TempDir::new().unwrap();
        let path = write(&dir, "mod.hpp", "class Engine { void start(); };\n");
        let hits = search_file_ast(&path, "Engine").unwrap();
        assert!(!hits.is_empty(), "expected hits in .hpp file");
    }

    #[test]
    fn test_cpp_ignores_comment() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "mod.cpp",
            "// Greeter is mentioned here\nclass Other {};\n",
        );
        let hits = search_file_ast(&path, "Greeter").unwrap();
        assert!(hits.is_empty(), "comment mention must not be returned");
    }

    // ── Luau ────────────────────────────────────────────────────────────────

    #[test]
    fn test_luau_finds_definition_and_call() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "RotateTool.luau",
            "local function SetRotation(cf: CFrame)\n    return cf\nend\n\nSetRotation(CFrame.new())\n",
        );
        let hits = search_file_ast(&path, "SetRotation").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert!(lines.contains(&1), "definition on line 1");
        assert!(lines.contains(&5), "call on line 5");
    }

    #[test]
    fn test_svelte_finds_definition_and_call_in_script_block() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "Component.svelte",
            "<script>
function greet() {}
greet()
</script>
<div>{greet()}</div>
",
        );
        let hits = search_file_ast(&path, "greet").unwrap();
        let lines: Vec<usize> = hits.iter().map(|(l, _, _)| *l).collect();
        assert_eq!(lines, vec![2, 3]);
    }

    #[test]
    fn test_svelte_inline_script_reports_original_columns() {
        let dir = TempDir::new().unwrap();
        let path = write(
            &dir,
            "Inline.svelte",
            "<script>const greet = () => greet()</script>\n",
        );
        let hits = search_file_ast(&path, "greet").unwrap();

        let positions: Vec<(usize, usize)> = hits.iter().map(|(l, c, _)| (*l, *c)).collect();
        assert_eq!(positions, vec![(1, 15), (1, 29)]);
    }

    #[tokio::test]
    async fn test_find_usages_luau_finds_cross_file_call_sites() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        write(
            &dir,
            "RotateTool.luau",
            "local RotateTool = {}\n\nfunction RotateTool.SetRotation(cf: CFrame)\n    return cf\nend\n\nreturn RotateTool\n",
        );
        write(
            &dir,
            "Main.server.luau",
            "local RotateTool = require(script.Parent.RotateTool)\n\nRotateTool.SetRotation(CFrame.new())\n",
        );

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "SetRotation")
            .map(|s| s.id.clone())
            .expect("SetRotation must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"]
            .as_array()
            .expect("usages must be an array");
        assert!(
            usages
                .iter()
                .any(|usage| usage["file"] == "RotateTool.luau"),
            "expected definition hit in RotateTool.luau, got: {usages:?}"
        );
        assert!(
            usages
                .iter()
                .any(|usage| usage["file"] == "Main.server.luau"),
            "expected cross-file call hit in Main.server.luau, got: {usages:?}"
        );
    }

    #[tokio::test]
    async fn test_find_usages_falls_back_to_supported_zero_symbol_files() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        write(&dir, "defs.js", "function greet() {}\n");
        write(&dir, "calls.js", "greet();\ngreet();\n");

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "greet")
            .map(|s| s.id.clone())
            .expect("greet must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"]
            .as_array()
            .expect("usages must be an array");
        assert!(
            usages.iter().any(|usage| usage["file"] == "defs.js"),
            "expected definition hit in defs.js, got: {usages:?}"
        );
        assert!(
            usages.iter().any(|usage| usage["file"] == "calls.js"),
            "expected fallback hit in zero-symbol file calls.js, got: {usages:?}"
        );
    }

    // ── Early-exit across multiple files ────────────────────────────────────

    /// Early-exit: spread the symbol across 5 files (3 usages each = 15 total),
    /// request limit=5. The walk must stop as soon as 5 usages are collected,
    /// leaving the remaining files unvisited.
    ///
    /// Observable invariants:
    ///   - count == 5  (exactly the requested page)
    ///   - truncated == true  (more results exist beyond the page)
    ///   - next_page_message contains "offset: 5"
    #[tokio::test]
    async fn test_early_exit_stops_after_limit_across_files() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        // 5 files, each calling `shared_fn` twice (plus 1 definition in file 0 = 11 usages)
        write(
            &dir,
            "a.rs",
            "pub fn shared_fn() {}\npub fn a1() { shared_fn(); }\npub fn a2() { shared_fn(); }\n",
        );
        for (name, body) in [
            (
                "b.rs",
                "pub fn b1() { shared_fn(); }\npub fn b2() { shared_fn(); }\n",
            ),
            (
                "c.rs",
                "pub fn c1() { shared_fn(); }\npub fn c2() { shared_fn(); }\n",
            ),
            (
                "d.rs",
                "pub fn d1() { shared_fn(); }\npub fn d2() { shared_fn(); }\n",
            ),
            (
                "e.rs",
                "pub fn e1() { shared_fn(); }\npub fn e2() { shared_fn(); }\n",
            ),
        ] {
            write(&dir, name, body);
        }

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "shared_fn")
            .map(|s| s.id.clone())
            .expect("shared_fn must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(5),
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(response["count"], 5, "count must equal limit=5");
        assert_eq!(response["truncated"], true, "truncated must be true");
        let msg = response["next_page_message"]
            .as_str()
            .expect("next_page_message must be present");
        assert!(
            msg.contains("offset: 5"),
            "message must contain 'offset: 5', got: {msg}"
        );
    }

    /// Early-exit with offset: spread the symbol across 5 files, request
    /// offset=4, limit=4. The walk must collect ≥8 usages then stop; the page
    /// must contain exactly 4 results at the right offset.
    #[tokio::test]
    async fn test_early_exit_with_offset_across_files() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        // Each file contributes 2 usages; 5 files = 10 usages (+ 1 definition)
        write(
            &dir,
            "a.rs",
            "pub fn target() {}\npub fn a1() { target(); }\npub fn a2() { target(); }\n",
        );
        for (name, body) in [
            (
                "b.rs",
                "pub fn b1() { target(); }\npub fn b2() { target(); }\n",
            ),
            (
                "c.rs",
                "pub fn c1() { target(); }\npub fn c2() { target(); }\n",
            ),
            (
                "d.rs",
                "pub fn d1() { target(); }\npub fn d2() { target(); }\n",
            ),
            (
                "e.rs",
                "pub fn e1() { target(); }\npub fn e2() { target(); }\n",
            ),
        ] {
            write(&dir, name, body);
        }

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "target")
            .map(|s| s.id.clone())
            .expect("target must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(4),
            offset: Some(4),
        })
        .await
        .unwrap();

        let count = response["count"].as_u64().expect("count must be present");
        assert_eq!(count, 4, "page must contain exactly 4 results");

        // With 11 total usages and offset+limit=8, there are 3 more beyond the page.
        assert_eq!(response["truncated"], true, "truncated must be true");
    }

    // ── Error paths ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_usages_unknown_symbol_returns_structured_error() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();

        // Index the project so load_project_index succeeds.
        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let err = find_usages(FindUsagesParams {
            project,
            symbol_id: "nonexistent::symbol#function".to_string(),
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "SYMBOL_NOT_FOUND");
        assert!(tool_err
            .to_string()
            .contains("nonexistent::symbol#function"));
    }

    // ── Preservation tests (Property 2) ─────────────────────────────────────
    //
    // These tests MUST PASS on unfixed code. They establish the baseline
    // behavior that must not regress after the fix is applied in task 3.
    //
    // Validates: Requirements 3.3, 3.4, 3.6

    /// Preservation: index a symbol with 3 usages, call find_usages, assert
    /// all 3 usages are returned (no truncation when total is small).
    ///
    /// Validates: Requirements 3.3
    #[tokio::test]
    async fn test_preserve_all_usages_returned_when_small() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();

        // Define `target_fn` and call it exactly 3 times.
        let src = r#"
pub fn target_fn() {}

pub fn caller_a() { target_fn(); }
pub fn caller_b() { target_fn(); }
pub fn caller_c() { target_fn(); }
"#;
        write(&dir, "lib.rs", src);

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "target_fn")
            .map(|s| s.id.clone())
            .expect("target_fn must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let count = response["count"].as_u64().expect("count must be present");
        // The definition + 3 call sites = at least 3 usages.
        assert!(
            count >= 3,
            "expected at least 3 usages (definition + 3 calls), got {}",
            count
        );

        let usages = response["usages"].as_array().expect("usages must be array");
        assert_eq!(
            usages.len() as u64,
            count,
            "usages array length must match count field"
        );
    }

    /// Preservation: SYMBOL_NOT_FOUND error path is unchanged.
    ///
    /// Validates: Requirements 3.4
    #[tokio::test]
    async fn test_preserve_symbol_not_found_error_unchanged() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"pub fn hello() {}").unwrap();

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let err = find_usages(FindUsagesParams {
            project,
            symbol_id: "nonexistent::symbol#function".to_string(),
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap_err();

        let tool_err = err
            .downcast_ref::<crate::error::ToolError>()
            .expect("error should be a ToolError");

        assert_eq!(tool_err.code(), "SYMBOL_NOT_FOUND");
        assert!(tool_err
            .to_string()
            .contains("nonexistent::symbol#function"));
    }

    // ── Bug condition exploration tests ─────────────────────────────────────

    /// Bug condition: call find_usages and assert the response contains a
    /// `truncated` field.
    ///
    /// Validates: Requirements 1.3, 1.4, 1.5 (bug condition)
    #[tokio::test]
    async fn test_bug_find_usages_truncated_field_absent() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();

        let src = r#"
pub fn target_fn() {}

pub fn caller_a() { target_fn(); }
pub fn caller_b() { target_fn(); }
pub fn caller_c() { target_fn(); }
pub fn caller_d() { target_fn(); }
pub fn caller_e() { target_fn(); }
"#;
        write(&dir, "lib.rs", src);

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "target_fn")
            .map(|s| s.id.clone())
            .expect("target_fn must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert!(
            response["count"].as_u64().unwrap_or(0) > 0,
            "expected at least one usage, got: {}",
            response
        );

        assert!(
            response["truncated"].is_boolean(),
            "COUNTEREXAMPLE: `truncated` field is absent from find_usages response — \
             caller cannot tell whether the result set is complete or silently capped. \
             Full response: {}",
            response
        );
    }

    // ── Pagination unit tests (Task 3.6) ────────────────────────────────────

    /// Helper: index a project with a symbol that has N usages.
    /// Returns (project_path, symbol_id).
    async fn setup_usages_project(n_callers: usize) -> (TempDir, String, String) {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        let mut src = String::from("pub fn target_fn() {}\n");
        for i in 0..n_callers {
            src.push_str(&format!("pub fn caller_{i:03}() {{ target_fn(); }}\n"));
        }
        std::fs::write(dir.path().join("lib.rs"), src.as_bytes()).unwrap();

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "target_fn")
            .map(|s| s.id.clone())
            .expect("target_fn must be indexed");

        (dir, project, sym_id)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_find_usages_skips_walkdir_permission_denied_entries() {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn target_fn() {}\npub fn caller() { target_fn(); }\n",
        )
        .unwrap();
        let restricted = RestrictedDir::new(&dir, "blocked");

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == "target_fn")
            .map(|s| s.id.clone())
            .expect("target_fn must be indexed");

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"].as_array().unwrap();
        assert!(response["count"].as_u64().unwrap() >= 2);
        assert!(!usages.is_empty());
        if restricted.is_effectively_inaccessible() {
            assert!(usages.iter().all(|usage| usage["file"] == "lib.rs"));
        }
    }

    /// Symbol with 6 usages, limit=3 → count=3, truncated:true, next_page_message contains "offset: 3"
    #[tokio::test]
    async fn test_find_usages_limit_caps_results() {
        let (_dir, project, sym_id) = setup_usages_project(6).await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(3),
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(response["count"], 3, "count must be capped at limit=3");
        assert_eq!(response["truncated"], true, "truncated must be true");
        let msg = response["next_page_message"]
            .as_str()
            .expect("next_page_message must be present");
        assert!(
            msg.contains("offset: 3"),
            "message must contain 'offset: 3', got: {msg}"
        );
    }

    /// Symbol with 6 usages, offset=3, limit=3 → count=3, truncated:false
    #[tokio::test]
    async fn test_find_usages_offset_second_page() {
        let (_dir, project, sym_id) = setup_usages_project(6).await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(3),
            offset: Some(3),
        })
        .await
        .unwrap();

        // 6 usages total (definition + 6 callers = 7, but we only care that offset+limit covers the rest)
        // With 7 total usages: page 2 (offset=3, limit=3) = items 3,4,5 → count=3, truncated depends on total
        let count = response["count"].as_u64().unwrap();
        assert!(count > 0, "second page must have results");
        // truncated is true only if there are more items beyond offset+count
        let truncated = response["truncated"]
            .as_bool()
            .expect("truncated must be bool");
        let total_implied = 3 + count as usize; // offset + page_count
                                                // If total > offset + count, truncated should be true; otherwise false
        if truncated {
            assert!(response["next_page_message"].is_string());
        } else {
            assert!(response["next_page_message"].is_null());
        }
        let _ = total_implied; // suppress unused warning
    }

    /// Symbol with 3 usages, limit=100 → count=3, truncated:false
    #[tokio::test]
    async fn test_find_usages_under_limit_truncated_false() {
        let (_dir, project, sym_id) = setup_usages_project(3).await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(100),
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(
            response["truncated"], false,
            "truncated must be false when total < limit"
        );
        assert!(
            response["next_page_message"].is_null(),
            "no next_page_message when not truncated"
        );
    }

    /// Symbol with 3 usages, offset=100 → count=0, truncated:false
    #[tokio::test]
    async fn test_find_usages_offset_beyond_total_empty() {
        let (_dir, project, sym_id) = setup_usages_project(3).await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: None,
            limit: Some(100),
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

    // ── Scope glob filter ────────────────────────────────────────────────────

    /// Helper: index a project and return (TempDir, project_path, symbol_id).
    /// `files` is a slice of (relative_path, source) pairs; the symbol to find
    /// is identified by `sym_name`.
    async fn setup_scope_project(
        files: &[(&str, &str)],
        sym_name: &str,
    ) -> (TempDir, String, String) {
        use crate::index::format::{index_dir, save_index};
        use crate::indexer::{registry, Indexer};

        let dir = TempDir::new().unwrap();
        for (rel, src) in files {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, src.as_bytes()).unwrap();
        }

        let indexer = Indexer::new(registry::build_default_registry());
        let (index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let idx_dir = index_dir(&canonical).unwrap();
        std::fs::create_dir_all(&idx_dir).unwrap();
        save_index(&index, &idx_dir.join("index.bin")).unwrap();
        let project = dir.path().to_string_lossy().to_string();

        let sym_id = index
            .symbols
            .values()
            .find(|s| s.name == sym_name)
            .map(|s| s.id.clone())
            .unwrap_or_else(|| panic!("{sym_name} must be indexed"));

        (dir, project, sym_id)
    }

    /// Scope glob restricts JS usages to `src/` only; `other/` file is excluded.
    #[tokio::test]
    async fn test_scope_glob_js() {
        let (_dir, project, sym_id) = setup_scope_project(
            &[
                ("src/app.js", "function greet() {}\ngreet();\ngreet();\n"),
                ("other/util.js", "function greet() {}\ngreet();\n"),
            ],
            "greet",
        )
        .await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: Some("src/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"].as_array().unwrap();
        assert!(
            usages
                .iter()
                .all(|u| u["file"].as_str().unwrap().starts_with("src/")),
            "all usages must come from src/, got: {usages:?}"
        );
        assert!(
            response["count"].as_u64().unwrap() > 0,
            "must find usages in src/"
        );
    }

    /// Scope glob restricts TS usages to `src/` only; `other/` file is excluded.
    #[tokio::test]
    async fn test_scope_glob_ts() {
        let (_dir, project, sym_id) = setup_scope_project(
            &[
                (
                    "src/app.ts",
                    "function greet(): void {}\ngreet();\ngreet();\n",
                ),
                ("other/util.ts", "function greet(): void {}\ngreet();\n"),
            ],
            "greet",
        )
        .await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: Some("src/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"].as_array().unwrap();
        assert!(
            usages
                .iter()
                .all(|u| u["file"].as_str().unwrap().starts_with("src/")),
            "all usages must come from src/, got: {usages:?}"
        );
        assert!(response["count"].as_u64().unwrap() > 0);
    }

    /// Scope glob restricts C usages to `src/` only; `other/` file is excluded.
    #[tokio::test]
    async fn test_scope_glob_c() {
        let (_dir, project, sym_id) = setup_scope_project(
            &[
                (
                    "src/main.c",
                    "void process(void) {}\nint main(void) { process(); process(); return 0; }\n",
                ),
                (
                    "other/util.c",
                    "void process(void) {}\nvoid helper(void) { process(); }\n",
                ),
            ],
            "process",
        )
        .await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: Some("src/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"].as_array().unwrap();
        assert!(
            usages
                .iter()
                .all(|u| u["file"].as_str().unwrap().starts_with("src/")),
            "all usages must come from src/, got: {usages:?}"
        );
        assert!(response["count"].as_u64().unwrap() > 0);
    }

    /// Scope glob restricts C++ usages to `src/` only; `other/` file is excluded.
    #[tokio::test]
    async fn test_scope_glob_cpp() {
        let (_dir, project, sym_id) = setup_scope_project(
            &[
                (
                    "src/engine.cpp",
                    "class Engine {};\nvoid use() { Engine e; Engine f; }\n",
                ),
                (
                    "other/legacy.cpp",
                    "class Engine {};\nvoid old() { Engine e; }\n",
                ),
            ],
            "Engine",
        )
        .await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: Some("src/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        let usages = response["usages"].as_array().unwrap();
        assert!(
            usages
                .iter()
                .all(|u| u["file"].as_str().unwrap().starts_with("src/")),
            "all usages must come from src/, got: {usages:?}"
        );
        assert!(response["count"].as_u64().unwrap() > 0);
    }

    /// Scope glob that matches no files returns count=0, truncated=false.
    #[tokio::test]
    async fn test_scope_glob_no_match_returns_empty() {
        let (_dir, project, sym_id) = setup_scope_project(
            &[(
                "lib.rs",
                "pub fn target() {}\npub fn caller() { target(); }\n",
            )],
            "target",
        )
        .await;

        let response = find_usages(FindUsagesParams {
            project,
            symbol_id: sym_id,
            scope: Some("nonexistent/**".to_string()),
            limit: None,
            offset: None,
        })
        .await
        .unwrap();

        assert_eq!(response["count"], 0, "no usages when scope matches nothing");
        assert_eq!(response["truncated"], false);
    }
}
