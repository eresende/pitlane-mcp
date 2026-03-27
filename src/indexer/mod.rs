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
                    if rel.components().any(|c| {
                        c.as_os_str().to_str().map_or(false, is_excluded_dir_name)
                    }) {
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
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");

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

    fn parse_file(
        &self,
        path: &Path,
        root: &Path,
    ) -> anyhow::Result<Vec<language::Symbol>> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

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

