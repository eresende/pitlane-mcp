pub mod format;

use crate::indexer::language::{Language, Symbol, SymbolId, SymbolKind};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
pub struct SymbolIndex {
    pub symbols: HashMap<SymbolId, Symbol>,
    pub by_file: HashMap<PathBuf, Vec<SymbolId>>,
    pub by_kind: HashMap<SymbolKind, Vec<SymbolId>>,
    pub by_language: HashMap<Language, Vec<SymbolId>>,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, symbol: Symbol) {
        let id = symbol.id.clone();
        self.by_file.entry(symbol.file.clone()).or_default().push(id.clone());
        self.by_kind.entry(symbol.kind.clone()).or_default().push(id.clone());
        self.by_language.entry(symbol.language.clone()).or_default().push(id.clone());
        self.symbols.insert(id, symbol);
    }

    pub fn remove_file(&mut self, path: &PathBuf) {
        if let Some(ids) = self.by_file.remove(path) {
            for id in &ids {
                if let Some(sym) = self.symbols.remove(id) {
                    if let Some(v) = self.by_kind.get_mut(&sym.kind) {
                        v.retain(|x| x != id);
                    }
                    if let Some(v) = self.by_language.get_mut(&sym.language) {
                        v.retain(|x| x != id);
                    }
                }
            }
        }
    }

    pub fn rebuild_secondary_indexes(&mut self) {
        self.by_file.clear();
        self.by_kind.clear();
        self.by_language.clear();
        let symbols: Vec<_> = self.symbols.values().cloned().collect();
        for sym in symbols {
            let id = sym.id.clone();
            self.by_file.entry(sym.file.clone()).or_default().push(id.clone());
            self.by_kind.entry(sym.kind.clone()).or_default().push(id.clone());
            self.by_language.entry(sym.language.clone()).or_default().push(id.clone());
        }
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::{Language, Symbol, SymbolKind, make_symbol_id};

    fn make_test_symbol(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        let path = PathBuf::from(file);
        let id = make_symbol_id(&path, name, &kind);
        Symbol {
            id,
            name: name.to_string(),
            qualified: name.to_string(),
            kind,
            language: Language::Rust,
            file: path,
            byte_start: 0,
            byte_end: 10,
            line_start: 1,
            line_end: 3,
            signature: None,
            doc: None,
        }
    }

    #[test]
    fn test_new_index_is_empty() {
        let index = SymbolIndex::new();
        assert_eq!(index.symbol_count(), 0);
        assert_eq!(index.file_count(), 0);
    }

    #[test]
    fn test_insert_and_counts() {
        let mut index = SymbolIndex::new();
        index.insert(make_test_symbol("Foo", SymbolKind::Struct, "src/lib.rs"));
        index.insert(make_test_symbol("Bar", SymbolKind::Struct, "src/lib.rs"));
        index.insert(make_test_symbol("baz", SymbolKind::Function, "src/other.rs"));

        assert_eq!(index.symbol_count(), 3);
        assert_eq!(index.file_count(), 2);
    }

    #[test]
    fn test_by_kind_index() {
        let mut index = SymbolIndex::new();
        index.insert(make_test_symbol("Foo", SymbolKind::Struct, "a.rs"));
        index.insert(make_test_symbol("Bar", SymbolKind::Struct, "a.rs"));
        index.insert(make_test_symbol("baz", SymbolKind::Function, "a.rs"));

        assert_eq!(index.by_kind[&SymbolKind::Struct].len(), 2);
        assert_eq!(index.by_kind[&SymbolKind::Function].len(), 1);
    }

    #[test]
    fn test_by_language_index() {
        let mut index = SymbolIndex::new();
        index.insert(make_test_symbol("foo", SymbolKind::Function, "a.rs"));

        assert_eq!(index.by_language[&Language::Rust].len(), 1);
    }

    #[test]
    fn test_remove_file() {
        let mut index = SymbolIndex::new();
        let path = PathBuf::from("src/lib.rs");
        index.insert(make_test_symbol("Foo", SymbolKind::Struct, "src/lib.rs"));
        index.insert(make_test_symbol("bar", SymbolKind::Function, "src/other.rs"));

        index.remove_file(&path);

        assert_eq!(index.symbol_count(), 1);
        assert_eq!(index.file_count(), 1);
        assert!(index.by_file.get(&path).is_none());
        // Remaining symbol is from other.rs
        assert!(index.symbols.values().all(|s| s.file != path));
    }

    #[test]
    fn test_remove_file_cleans_kind_index() {
        let mut index = SymbolIndex::new();
        let path = PathBuf::from("a.rs");
        index.insert(make_test_symbol("Foo", SymbolKind::Struct, "a.rs"));

        index.remove_file(&path);

        assert!(index.by_kind.get(&SymbolKind::Struct).map_or(true, |v| v.is_empty()));
    }

    #[test]
    fn test_rebuild_secondary_indexes() {
        let mut index = SymbolIndex::new();
        index.insert(make_test_symbol("Foo", SymbolKind::Struct, "a.rs"));
        index.insert(make_test_symbol("bar", SymbolKind::Function, "b.rs"));

        // Corrupt secondary indexes
        index.by_file.clear();
        index.by_kind.clear();
        index.by_language.clear();

        index.rebuild_secondary_indexes();

        assert_eq!(index.file_count(), 2);
        assert_eq!(index.by_kind[&SymbolKind::Struct].len(), 1);
        assert_eq!(index.by_kind[&SymbolKind::Function].len(), 1);
    }
}
