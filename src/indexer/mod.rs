pub mod language;
pub mod python;
pub mod registry;
pub mod rust;

use std::collections::HashMap;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};
use walkdir::WalkDir;

use crate::index::SymbolIndex;
use language::LanguageParser;

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

    /// Index a full project directory
    pub fn index_project(
        &self,
        root: &Path,
        exclude_patterns: &[String],
    ) -> anyhow::Result<(SymbolIndex, usize)> {
        let exclude_set = Self::build_exclude_set(exclude_patterns)?;
        let mut index = SymbolIndex::new();
        let mut file_count = 0usize;

        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                let path = e.path();
                let rel = match path.strip_prefix(root) {
                    Ok(r) => r,
                    Err(_) => return true,
                };
                // Always include root itself
                if rel == Path::new("") {
                    return true;
                }
                let rel_str = rel.to_string_lossy();
                // Exclude if matches any glob pattern
                if exclude_set.is_match(rel_str.as_ref()) {
                    return false;
                }
                // For directories: check trailing-slash variant and well-known names
                if e.file_type().is_dir() {
                    if exclude_set.is_match(format!("{}/", rel_str).as_str()) {
                        return false;
                    }
                    // Exclude any directory whose name is a well-known dependency/build dir
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
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Check file extension
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

            if !self.ext_map.contains_key(ext) {
                continue;
            }

            // Check file-level exclusion
            let rel = path.strip_prefix(root).unwrap_or(path);
            let rel_str = rel.to_string_lossy();
            if exclude_set.is_match(rel_str.as_ref()) || exclude_set.is_match(path) {
                continue;
            }

            if let Ok(symbols) = self.parse_file(path, root) {
                for symbol in symbols {
                    index.insert(symbol);
                }
                file_count += 1;
            }
        }

        Ok((index, file_count))
    }

    /// Re-index a single file (for incremental updates)
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

        let parser_idx = match self.ext_map.get(ext) {
            Some(idx) => *idx,
            None => return Ok(vec![]),
        };

        let source = std::fs::read(path)?;
        let lang_parser = &self.parsers[parser_idx];

        let mut ts_parser = tree_sitter::Parser::new();
        let ts_lang = match lang_parser.language() {
            language::Language::Rust => tree_sitter_rust::language(),
            language::Language::Python => tree_sitter_python::language(),
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
            sym.file = path.to_path_buf();
        }

        Ok(symbols)
    }
}

/// Returns true if a directory component name is a well-known dependency or build directory
/// that should never be indexed, regardless of where in the tree it appears.
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
    fn test_index_project_multiple_files() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"fn func_a() {}").unwrap();
        std::fs::write(dir.path().join("b.py"), b"def func_b():\n    pass\n").unwrap();

        let (index, file_count) = create_indexer().index_project(dir.path(), &[]).unwrap();

        assert_eq!(file_count, 2);
        assert_eq!(index.symbol_count(), 2);
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
}
