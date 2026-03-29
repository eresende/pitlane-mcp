use std::collections::HashSet;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::tools::index_project::load_project_index;

pub struct FindUsagesParams {
    pub project: String,
    pub symbol_id: String,
    pub scope: Option<String>,
}

pub async fn find_usages(params: FindUsagesParams) -> anyhow::Result<Value> {
    let index = load_project_index(&params.project)?;

    let sym = index
        .symbols
        .get(&params.symbol_id)
        .ok_or_else(|| anyhow::anyhow!("Symbol not found: {}", params.symbol_id))?;

    let symbol_name = sym.name.clone();
    let project_path = Path::new(&params.project)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&params.project).to_path_buf());

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

    let mut usages = Vec::new();

    // Walk all source files in the project
    for entry in WalkDir::new(&project_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "rs" && ext != "py" {
            continue;
        }

        // Apply scope filter
        if let Some(ref set) = scope_set {
            let rel = path.strip_prefix(&project_path).unwrap_or(path);
            if !set.is_match(rel) && !set.is_match(path) {
                continue;
            }
        }

        match search_file_ast(path, &symbol_name) {
            Ok(hits) => {
                for (line_num, col, snippet) in hits {
                    let rel = path.strip_prefix(&project_path).unwrap_or(path);
                    usages.push(json!({
                        "file": rel.to_string_lossy(),
                        "line": line_num,
                        "column": col,
                        "snippet": snippet,
                    }));
                }
            }
            Err(_) => continue,
        }
    }

    Ok(json!({
        "symbol_id": params.symbol_id,
        "symbol_name": symbol_name,
        "usages": usages,
        "count": usages.len(),
    }))
}

/// Searches `path` for AST nodes whose text equals `name`. Only true identifier
/// nodes are matched — string literals, comments, and substrings of longer
/// identifiers are never returned.
fn search_file_ast(path: &Path, name: &str) -> anyhow::Result<Vec<(usize, usize, String)>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    let ts_lang: tree_sitter::Language = match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => return Ok(vec![]),
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
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
}
