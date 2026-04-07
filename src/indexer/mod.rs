pub mod bash;
pub mod c;
pub mod cpp;
pub mod csharp;
pub mod go;
pub mod java;
pub mod javascript;
pub mod kotlin;
pub mod language;
pub mod lua;
pub mod objc;
pub mod php;
pub mod python;
pub mod registry;
pub mod ruby;
pub mod rust;
pub mod swift;
pub mod typescript;
pub mod zig;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use tracing::{debug, warn};
use walkdir::WalkDir;

use crate::index::SymbolIndex;
use language::LanguageParser;

/// Files larger than this are skipped to avoid memory exhaustion and parser hangs.
const MAX_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB

pub struct Indexer {
    parsers: Vec<Box<dyn LanguageParser>>,
    /// Map from file extension to parser index
    ext_map: HashMap<String, usize>,
}

impl Indexer {
    pub fn new(parsers: Vec<Box<dyn LanguageParser>>) -> Self {
        let mut ext_map = HashMap::new();
        for (i, parser) in parsers.iter().enumerate() {
            for ext in parser.extensions() {
                ext_map.insert(ext.to_string(), i);
            }
        }
        Self { parsers, ext_map }
    }

    fn build_exclude_set(patterns: &[String]) -> anyhow::Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pat in patterns {
            builder.add(Glob::new(pat)?);
        }
        Ok(builder.build()?)
    }

    /// Index a full project directory.
    ///
    /// If `on_progress` is provided it is called twice:
    ///   1. After Phase 1 (file collection) with `(0, total_files)` — signals start.
    ///   2. After Phase 2 (parsing) with `(total_files, total_files)` — signals completion.
    pub fn index_project(
        &self,
        root: &Path,
        exclude_patterns: &[String],
    ) -> anyhow::Result<(SymbolIndex, usize)> {
        self.index_project_with_progress(root, exclude_patterns, usize::MAX, None)
    }

    /// Like `index_project` but accepts an optional progress callback and a file-count cap.
    ///
    /// `max_files` limits how many eligible source files will be accepted before the walk
    /// aborts with `ToolError::FileLimitExceeded`. The walk stops as soon as `max_files + 1`
    /// eligible files are found, so it never reads the full filesystem for large paths.
    /// Pass `usize::MAX` to disable the cap.
    pub fn index_project_with_progress(
        &self,
        root: &Path,
        exclude_patterns: &[String],
        max_files: usize,
        on_progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> anyhow::Result<(SymbolIndex, usize)> {
        let exclude_set = Self::build_exclude_set(exclude_patterns)?;

        // Phase 1 — collect eligible file paths (sequential: WalkDir is not parallel).
        let eligible: Vec<std::path::PathBuf> = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let path = e.path();
                let rel = match path.strip_prefix(root) {
                    Ok(r) => r,
                    Err(_) => return true,
                };
                if rel == Path::new("") {
                    return true;
                }
                let rel_str = rel.to_string_lossy();
                if exclude_set.is_match(rel_str.as_ref()) {
                    return false;
                }
                if e.file_type().is_dir() {
                    if exclude_set.is_match(format!("{}/", rel_str).as_str()) {
                        return false;
                    }
                    if rel
                        .components()
                        .any(|c| c.as_os_str().to_str().is_some_and(is_excluded_dir_name))
                    {
                        return false;
                    }
                }
                true
            })
            .filter_map(|e| e.ok())
            .filter(|e| {
                let path = e.path();
                if !path.is_file() {
                    return false;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !self.ext_map.contains_key(ext) {
                    return false;
                }
                if is_declaration_file(path) {
                    return false;
                }
                let rel = path.strip_prefix(root).unwrap_or(path);
                let rel_str = rel.to_string_lossy();
                if exclude_set.is_match(rel_str.as_ref()) || exclude_set.is_match(path) {
                    return false;
                }
                if e.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                    warn!(file = %rel_str, limit = MAX_FILE_BYTES, "skipping oversized file");
                    return false;
                }
                true
            })
            .take(max_files.saturating_add(1))
            .map(|e| e.into_path())
            .collect();

        if eligible.len() > max_files {
            return Err(crate::error::ToolError::FileLimitExceeded {
                path: root.display().to_string(),
                limit: max_files,
            }
            .into());
        }

        let total_files = eligible.len();

        // Notify: Phase 1 complete — we now know the total file count.
        if let Some(cb) = on_progress {
            cb(0, total_files);
        }

        // Phase 2 — parse files in parallel.
        // Each parse_file call is CPU-bound and independent: it creates its own
        // tree-sitter Parser and reads a single file. Thread count is capped at
        // min(available_parallelism, 16) to avoid OOM on very large codebases
        // (e.g. LLVM). Stack size is bumped to 64 MiB per thread: tree-sitter
        // recurses deeply into heavily-templated C++ and overflows the default 8 MiB.
        let threads = std::thread::available_parallelism()
            .map(|n| n.get().min(16))
            .unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(64 * 1024 * 1024)
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().unwrap());

        // Throttle progress ticks: fire every ~5% of files, minimum every 10 files,
        // but at least once. This keeps notification overhead negligible even for
        // large codebases while still giving visible progress for small ones.
        let tick_interval = ((total_files / 20).max(10)).max(1);
        let completed = std::sync::atomic::AtomicUsize::new(0);

        let parsed: Vec<Vec<language::Symbol>> = pool.install(|| {
            eligible
                .par_iter()
                .filter_map(|path| {
                    let result = self.parse_file(path, root).ok();
                    if let Some(cb) = on_progress {
                        let prev = completed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let current = prev + 1;
                        // Fire on every tick_interval boundary.
                        if current.is_multiple_of(tick_interval) {
                            cb(current, total_files);
                        }
                    }
                    result
                })
                .collect()
        });

        // Phase 3 — insert symbols into the index (sequential: SymbolIndex is &mut).
        let mut index = SymbolIndex::new();
        let file_count = parsed.len();
        for symbols in parsed {
            for symbol in symbols {
                index.insert(symbol);
            }
        }

        // Notify: Phase 2+3 complete (always fire the final tick).
        if let Some(cb) = on_progress {
            cb(file_count, total_files);
        }

        Ok((index, file_count))
    }

    /// Re-index a single file
    pub fn reindex_file(
        &self,
        file_path: &Path,
        root: &Path,
        index: &mut SymbolIndex,
    ) -> anyhow::Result<()> {
        // Remove existing symbols for this file
        let abs_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            root.join(file_path)
        };

        index.remove_file(&abs_path);

        // Re-parse and insert
        if abs_path.exists() {
            let symbols = self.parse_file(&abs_path, root)?;
            for symbol in symbols {
                index.insert(symbol);
            }
        }

        Ok(())
    }

    fn parse_file(&self, path: &Path, root: &Path) -> anyhow::Result<Vec<language::Symbol>> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        // Skip TypeScript declaration files (*.d.ts, *.d.mts, *.d.cts).
        // Also enforced in the eligible filter of index_project; this guard
        // covers the reindex_file path.
        if is_declaration_file(path) {
            return Ok(vec![]);
        }

        let parser_idx = match self.ext_map.get(ext) {
            Some(idx) => *idx,
            None => return Ok(vec![]),
        };

        // Guard against oversized files (e.g. minified bundles, generated data dicts)
        let file_size = path.metadata().map(|m| m.len()).unwrap_or(0);
        if file_size > MAX_FILE_BYTES {
            warn!(file = %path.display(), limit = MAX_FILE_BYTES, "skipping oversized file");
            return Ok(vec![]);
        }

        debug!(file = %path.display(), bytes = file_size, "parsing file");
        let source = std::fs::read(path)?;
        let lang_parser = &self.parsers[parser_idx];

        let mut ts_parser = tree_sitter::Parser::new();
        let ts_lang = match tree_sitter_language_for_extension(ext) {
            Some(lang) => lang,
            None => return Ok(vec![]),
        };
        ts_parser.set_language(&ts_lang)?;

        let tree = match ts_parser.parse(&source, None) {
            Some(t) => t,
            None => return Ok(vec![]),
        };

        // Use relative path for symbol IDs if possible
        let rel_path = path.strip_prefix(root).unwrap_or(path);

        let mut symbols = lang_parser.extract_symbols(&source, &tree, rel_path);

        // Update file to absolute path
        for sym in &mut symbols {
            sym.file = Arc::new(path.to_path_buf());
        }

        Ok(symbols)
    }
}

/// Returns true if a directory component name is a well-known dependency or build directory
/// that should never be indexed, regardless of where in the tree it appears.
/// Reads the `.gitignore` at `root` and converts each entry into glob patterns
/// compatible with the indexer's `GlobSet`-based exclusion system.
///
/// Handles the most common gitignore syntax:
/// - Blank lines and `#` comments are skipped.
/// - `!negation` patterns are skipped (too complex to invert reliably).
/// - `/pattern` (root-anchored) → `pattern` + `pattern/**`
/// - `pattern/` (dir-only) or plain name → `**/pattern` + `**/pattern/**`
/// - `path/with/sep` (relative path) → as-is + `path/with/sep/**`
/// - Globs containing `*` (e.g. `*.pyc`) → `**/*.pyc` only (no `/**` variant)
///
/// Returns an empty `Vec` if no `.gitignore` exists or it cannot be read.
pub fn load_gitignore_patterns(root: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(root.join(".gitignore")) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut patterns = Vec::new();

    for raw in content.lines() {
        let line = raw.trim();

        // Skip blank lines, comments, and negation entries.
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }

        let (anchored, pat) = match line.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, line),
        };

        let dir_only = pat.ends_with('/');
        let pat = pat.trim_end_matches('/');

        if pat.is_empty() {
            continue;
        }

        if anchored {
            // Root-relative: match only from the project root.
            patterns.push(pat.to_string());
            patterns.push(format!("{}/**", pat));
        } else if pat.contains('/') {
            // Explicit sub-path (e.g. `packages/generated`): match as-is.
            patterns.push(pat.to_string());
            patterns.push(format!("{}/**", pat));
        } else {
            // Simple name or glob: match at any depth.
            patterns.push(format!("**/{}", pat));
            // Add a directory-contents variant unless the pattern is a file glob.
            if !pat.contains('*') || dir_only {
                patterns.push(format!("**/{}/**", pat));
            }
        }
    }

    patterns
}

/// Returns true for TypeScript declaration files (`*.d.ts`, `*.d.mts`, `*.d.cts`).
/// These are generated artifacts and should not be indexed.
pub fn is_declaration_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(ext, "ts" | "mts" | "cts")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.ends_with(".d"))
}

/// Returns true if `ext` is a file extension supported by the indexer.
/// Single source of truth — update this when adding a new language.
pub fn is_supported_extension(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "py"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "mts"
            | "cts"
            | "c"
            | "h"
            | "cpp"
            | "cc"
            | "cxx"
            | "hpp"
            | "hxx"
            | "go"
            | "java"
            | "sh"
            | "bash"
            | "cs"
            | "rb"
            | "swift"
            | "m"
            | "mm"
            | "php"
            | "zig"
            | "kt"
            | "kts"
            | "luau"
            | "lua"
    )
}

/// Returns the tree-sitter language for a supported extension.
/// Keep this in sync with `is_supported_extension` so indexing and usage search
/// agree on which parser to use for each file type.
pub fn tree_sitter_language_for_extension(ext: &str) -> Option<tree_sitter::Language> {
    Some(match ext {
        "rs" => tree_sitter_rust::LANGUAGE.into(),
        "py" => tree_sitter_python::LANGUAGE.into(),
        "js" | "jsx" | "mjs" | "cjs" => tree_sitter_javascript::LANGUAGE.into(),
        "ts" | "mts" | "cts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => tree_sitter_cpp::LANGUAGE.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "sh" | "bash" => tree_sitter_bash::LANGUAGE.into(),
        "cs" => tree_sitter_c_sharp::LANGUAGE.into(),
        "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "m" | "mm" => tree_sitter_objc::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "zig" => tree_sitter_zig::LANGUAGE.into(),
        "kt" | "kts" => tree_sitter_kotlin_ng::LANGUAGE.into(),
        "luau" | "lua" => tree_sitter_luau::LANGUAGE.into(),
        _ => return None,
    })
}

pub fn is_excluded_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".venv"
            | "venv"
            | ".env"
            | "env"
            | "node_modules"
            | "__pycache__"
            | ".git"
            | ".hg"
            | ".svn"
            | "site-packages"
            | "dist-packages"
            | ".tox"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".eggs"
            | ".cache"
            | ".idea"
            | ".vscode"
            | "target"
            // JavaScript / TypeScript build and cache output
            | "dist"
            | "build"
            | "out"
            | "coverage"
            | ".next"
            | ".nuxt"
            | ".turbo"
            | ".svelte-kit"
            | ".parcel-cache"
            | "storybook-static"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::registry;
    use tempfile::TempDir;

    fn create_indexer() -> Indexer {
        Indexer::new(registry::build_default_registry())
    }

    #[test]
    fn test_is_excluded_dir_name_known() {
        for name in &[
            "target",
            "node_modules",
            ".git",
            "__pycache__",
            ".venv",
            "venv",
            ".mypy_cache",
            // JS build dirs
            "dist",
            "build",
            "out",
            "coverage",
            ".next",
            ".nuxt",
            ".turbo",
            ".svelte-kit",
            ".parcel-cache",
            "storybook-static",
        ] {
            assert!(is_excluded_dir_name(name), "{name} should be excluded");
        }
    }

    #[test]
    fn test_is_excluded_dir_name_unknown() {
        for name in &["src", "lib", "tests", "docs", "my_module"] {
            assert!(!is_excluded_dir_name(name), "{name} should not be excluded");
        }
    }

    #[test]
    fn test_index_project_rust_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            b"pub fn hello() {}\npub struct World;",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 2);
    }

    #[test]
    fn test_index_project_python_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("main.py"),
            b"def foo():\n    pass\n\nclass Bar:\n    pass\n",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 2);
    }

    #[test]
    fn test_index_project_luau_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("init.luau"),
            b"local function greet()\nend\nexport type Point = { x: number, y: number }\n",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"Point"));
    }

    #[test]
    fn test_index_project_lua_extension_uses_luau_parser() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("module.lua"),
            b"function Greeter:speak()\nend\n",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let method = index
            .symbols
            .values()
            .find(|s| s.name == "speak")
            .expect("Lua method should be indexed");
        assert_eq!(method.language, language::Language::Lua);
    }

    #[test]
    fn test_index_project_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"fn func_a() {}").unwrap();
        std::fs::write(dir.path().join("b.py"), b"def func_b():\n    pass\n").unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 2);
        assert_eq!(index.symbol_count(), 2);
    }

    #[test]
    fn test_index_project_js_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("app.js"),
            b"function hello() {}\nconst greet = () => {};",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 2);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"hello"));
        assert!(names.contains(&"greet"));
    }

    #[test]
    fn test_index_project_ts_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.ts"),
            b"interface Foo {}\ntype Bar = string;\nfunction baz(): void {}",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 3);
    }

    #[test]
    fn test_index_project_c_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.c"),
            b"#define MAX 100\ntypedef int Status;\nstruct Node { int val; };\nenum Dir { NORTH, SOUTH };\nint process(int x) { return x; }",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MAX"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Node"));
        assert!(names.contains(&"Dir"));
        assert!(names.contains(&"process"));
    }

    #[test]
    fn test_index_project_c_header_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("utils.h"),
            b"typedef unsigned int uint32;\nstruct Config { int debug; };\nvoid init(void);",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"uint32"));
        assert!(names.contains(&"Config"));
    }

    #[test]
    fn test_index_project_cpp_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("greeter.cpp"),
            b"class Greeter {\npublic:\n    void hello() {}\n    void bye() {}\n};\nint main() { return 0; }",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Greeter"), "class should be extracted");
        assert!(names.contains(&"hello"), "method should be extracted");
        assert!(names.contains(&"bye"), "method should be extracted");
        assert!(names.contains(&"main"));
    }

    #[test]
    fn test_index_project_cpp_header_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("math.hpp"),
            b"namespace math {\n    int add(int a, int b) { return a + b; }\n}\n",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"add"),
            "function in namespace should be extracted"
        );
        assert!(
            !names.contains(&"math"),
            "namespace itself should not be a symbol"
        );
    }

    #[test]
    fn test_index_project_ruby_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("greeter.rb"),
            b"module Greetable\n  def greet(name)\n  end\nend\n\nclass Greeter\n  include Greetable\n\n  def self.create\n  end\nend\n",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Greetable"), "module should be extracted");
        assert!(
            names.contains(&"greet"),
            "instance method should be extracted"
        );
        assert!(names.contains(&"Greeter"), "class should be extracted");
        assert!(
            names.contains(&"create"),
            "singleton method should be extracted"
        );
    }

    #[test]
    fn test_index_project_skips_dts_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.ts"), b"function real(): void {}").unwrap();
        std::fs::write(
            dir.path().join("lib.d.ts"),
            b"declare function real(): void;",
        )
        .unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        // Only lib.ts counts; lib.d.ts is silently skipped
        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 1);
    }

    #[test]
    fn test_index_project_excludes_dist_dir() {
        let dir = TempDir::new().unwrap();
        let dist = dir.path().join("dist");
        std::fs::create_dir(&dist).unwrap();
        std::fs::write(dist.join("bundle.js"), b"function compiled() {}").unwrap();
        std::fs::write(dir.path().join("src.js"), b"function source() {}").unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"source"));
        assert!(!names.contains(&"compiled"));
    }

    #[test]
    fn test_index_project_skips_unknown_extensions() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"hello world").unwrap();
        std::fs::write(dir.path().join("lib.rs"), b"fn hello() {}").unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        assert_eq!(index.symbol_count(), 1);
    }

    #[test]
    fn test_index_project_excludes_target_dir() {
        let dir = TempDir::new().unwrap();
        let target_dir = dir.path().join("target");
        std::fs::create_dir(&target_dir).unwrap();
        std::fs::write(target_dir.join("generated.rs"), b"fn generated() {}").unwrap();
        std::fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"generated"));
    }

    #[test]
    fn test_index_project_custom_exclude_pattern() {
        let dir = TempDir::new().unwrap();
        let vendor_dir = dir.path().join("vendor");
        std::fs::create_dir(&vendor_dir).unwrap();
        std::fs::write(vendor_dir.join("dep.rs"), b"fn dep_fn() {}").unwrap();
        std::fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();

        let excludes = vec!["vendor/**".to_string()];
        let (index, file_count) = create_indexer()
            .index_project(dir.path(), &excludes)
            .unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"dep_fn"));
    }

    #[test]
    fn test_reindex_file_replaces_symbols() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, b"fn original() {}").unwrap();

        let indexer = create_indexer();
        let (mut index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        assert_eq!(index.symbol_count(), 1);

        std::fs::write(&file_path, b"fn updated() {}\nfn added() {}").unwrap();
        indexer
            .reindex_file(&file_path, dir.path(), &mut index)
            .unwrap();

        assert_eq!(index.symbol_count(), 2);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"updated"));
        assert!(names.contains(&"added"));
        assert!(!names.contains(&"original"));
    }

    #[test]
    fn test_reindex_file_deleted_removes_symbols() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, b"fn foo() {}").unwrap();

        let indexer = create_indexer();
        let (mut index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        assert_eq!(index.symbol_count(), 1);

        std::fs::remove_file(&file_path).unwrap();
        indexer
            .reindex_file(&file_path, dir.path(), &mut index)
            .unwrap();

        assert_eq!(index.symbol_count(), 0);
    }

    #[test]
    fn test_index_project_skips_oversized_file() {
        let dir = TempDir::new().unwrap();
        // Normal file that should be indexed
        std::fs::write(dir.path().join("small.rs"), b"fn small() {}").unwrap();
        // Oversized file: MAX_FILE_BYTES + 1 bytes
        let big_content = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        std::fs::write(dir.path().join("big.rs"), &big_content).unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1, "oversized file should not count as indexed");
        assert_eq!(index.symbol_count(), 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"small"));
    }

    #[test]
    fn test_index_project_allows_file_at_size_limit() {
        let dir = TempDir::new().unwrap();
        // A file exactly at the limit should be indexed, not skipped
        let at_limit = vec![b' '; MAX_FILE_BYTES as usize];
        std::fs::write(dir.path().join("exact.rs"), &at_limit).unwrap();

        let (_, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 1, "file at exactly the limit should be indexed");
    }

    #[test]
    fn test_reindex_file_skips_oversized_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("lib.rs");
        std::fs::write(&file_path, b"fn original() {}").unwrap();

        let indexer = create_indexer();
        let (mut index, _) = indexer.index_project(dir.path(), &[]).unwrap();
        assert_eq!(index.symbol_count(), 1);

        // Replace with an oversized file
        let big_content = vec![b'x'; MAX_FILE_BYTES as usize + 1];
        std::fs::write(&file_path, &big_content).unwrap();
        indexer
            .reindex_file(&file_path, dir.path(), &mut index)
            .unwrap();

        // Previous symbols removed, oversized file produces no new symbols
        assert_eq!(index.symbol_count(), 0);
    }

    // ── load_gitignore_patterns ──────────────────────────────────────────────

    #[test]
    fn test_gitignore_missing_returns_empty() {
        let dir = TempDir::new().unwrap();
        assert!(load_gitignore_patterns(dir.path()).is_empty());
    }

    #[test]
    fn test_gitignore_skips_comments_and_blanks() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "# comment\n\n  \n").unwrap();
        assert!(load_gitignore_patterns(dir.path()).is_empty());
    }

    #[test]
    fn test_gitignore_skips_negation() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "!important.rs\n").unwrap();
        assert!(load_gitignore_patterns(dir.path()).is_empty());
    }

    #[test]
    fn test_gitignore_simple_name() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "cdk.out\n").unwrap();
        let pats = load_gitignore_patterns(dir.path());
        assert!(pats.contains(&"**/cdk.out".to_string()));
        assert!(pats.contains(&"**/cdk.out/**".to_string()));
    }

    #[test]
    fn test_gitignore_dir_only_trailing_slash() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
        let pats = load_gitignore_patterns(dir.path());
        assert!(pats.contains(&"**/dist".to_string()));
        assert!(pats.contains(&"**/dist/**".to_string()));
    }

    #[test]
    fn test_gitignore_root_anchored() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "/build\n").unwrap();
        let pats = load_gitignore_patterns(dir.path());
        assert!(pats.contains(&"build".to_string()));
        assert!(pats.contains(&"build/**".to_string()));
        // Must NOT produce a `**/build` variant
        assert!(!pats.contains(&"**/build".to_string()));
    }

    #[test]
    fn test_gitignore_file_glob_no_dir_variant() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "*.pyc\n").unwrap();
        let pats = load_gitignore_patterns(dir.path());
        assert!(pats.contains(&"**/*.pyc".to_string()));
        // Should not produce a `**/*.pyc/**` variant
        assert!(!pats.contains(&"**/*.pyc/**".to_string()));
    }

    #[test]
    fn test_gitignore_sub_path() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "packages/generated\n").unwrap();
        let pats = load_gitignore_patterns(dir.path());
        assert!(pats.contains(&"packages/generated".to_string()));
        assert!(pats.contains(&"packages/generated/**".to_string()));
    }

    #[test]
    fn test_index_project_respects_gitignore() {
        let dir = TempDir::new().unwrap();

        // Source file that should be indexed
        std::fs::write(dir.path().join("main.rs"), b"fn main() {}").unwrap();

        // Directory that .gitignore excludes
        let cdk_out = dir.path().join("cdk.out");
        std::fs::create_dir(&cdk_out).unwrap();
        std::fs::write(cdk_out.join("generated.rs"), b"fn generated() {}").unwrap();

        std::fs::write(dir.path().join(".gitignore"), "cdk.out/\n").unwrap();

        let gitignore_pats = load_gitignore_patterns(dir.path());
        let (index, file_count) = create_indexer()
            .index_project(dir.path(), &gitignore_pats)
            .unwrap();

        assert_eq!(file_count, 1);
        let names: Vec<_> = index.symbols.values().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(!names.contains(&"generated"));
    }

    #[test]
    fn test_index_project_enforces_max_files_cap() {
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("f{i}.rs")), b"fn f() {}").unwrap();
        }

        // Cap of 3 should fail: we have 5 eligible files.
        let err = create_indexer()
            .index_project_with_progress(dir.path(), &[], 3, None)
            .unwrap_err();
        let tool_err = err.downcast_ref::<crate::error::ToolError>().unwrap();
        assert!(matches!(
            tool_err,
            crate::error::ToolError::FileLimitExceeded { limit: 3, .. }
        ));

        // Cap of 5 should succeed.
        let (_, file_count) = create_indexer()
            .index_project_with_progress(dir.path(), &[], 5, None)
            .unwrap();
        assert_eq!(file_count, 5);
    }
}
