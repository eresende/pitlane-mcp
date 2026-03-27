use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub type SymbolId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    // Rust
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Impl,
    Mod,
    Macro,
    Const,
    TypeAlias,
    // Python
    Class,
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Impl => "impl",
            SymbolKind::Mod => "mod",
            SymbolKind::Macro => "macro",
            SymbolKind::Const => "const",
            SymbolKind::TypeAlias => "type_alias",
            SymbolKind::Class => "class",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for SymbolKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "function" => Ok(SymbolKind::Function),
            "method" => Ok(SymbolKind::Method),
            "struct" => Ok(SymbolKind::Struct),
            "enum" => Ok(SymbolKind::Enum),
            "trait" => Ok(SymbolKind::Trait),
            "impl" => Ok(SymbolKind::Impl),
            "mod" => Ok(SymbolKind::Mod),
            "macro" => Ok(SymbolKind::Macro),
            "const" => Ok(SymbolKind::Const),
            "type_alias" | "typealias" => Ok(SymbolKind::TypeAlias),
            "class" => Ok(SymbolKind::Class),
            _ => Err(anyhow::anyhow!("Unknown symbol kind: {}", s)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub qualified: String,
    pub kind: SymbolKind,
    pub language: Language,
    pub file: PathBuf,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

pub fn make_symbol_id(relative_path: &std::path::Path, qualified: &str, kind: &SymbolKind) -> SymbolId {
    format!("{}::{}#{}", relative_path.display(), qualified, kind)
}

pub trait LanguageParser: Send + Sync {
    fn language(&self) -> Language;
    fn extensions(&self) -> &[&str];
    fn extract_symbols(
        &self,
        source: &[u8],
        tree: &tree_sitter::Tree,
        path: &std::path::Path,
    ) -> Vec<Symbol>;
}
