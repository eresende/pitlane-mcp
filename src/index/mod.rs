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
