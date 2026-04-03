use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

pub type SymbolId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    C,
    Cpp,
    Go,
    Java,
    Bash,
    CSharp,
    Ruby,
    Swift,
    ObjC,
    Php,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::Python => write!(f, "python"),
            Language::JavaScript => write!(f, "javascript"),
            Language::TypeScript => write!(f, "typescript"),
            Language::C => write!(f, "c"),
            Language::Cpp => write!(f, "cpp"),
            Language::Go => write!(f, "go"),
            Language::Java => write!(f, "java"),
            Language::Bash => write!(f, "bash"),
            Language::CSharp => write!(f, "csharp"),
            Language::Ruby => write!(f, "ruby"),
            Language::Swift => write!(f, "swift"),
            Language::ObjC => write!(f, "objc"),
            Language::Php => write!(f, "php"),
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
    // Python, Java
    Class,
    // TypeScript, Java
    Interface,
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
            SymbolKind::Interface => "interface",
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
            "interface" => Ok(SymbolKind::Interface),
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
    pub file: Arc<PathBuf>,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

pub fn make_symbol_id(
    relative_path: &std::path::Path,
    qualified: &str,
    kind: &SymbolKind,
) -> SymbolId {
    format!(
        "{}::{}#{}",
        relative_path.to_string_lossy().replace('\\', "/"),
        qualified,
        kind
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_language_display() {
        assert_eq!(Language::Rust.to_string(), "rust");
        assert_eq!(Language::Python.to_string(), "python");
        assert_eq!(Language::C.to_string(), "c");
        assert_eq!(Language::Cpp.to_string(), "cpp");
    }

    #[test]
    fn test_symbol_kind_display() {
        assert_eq!(SymbolKind::Function.to_string(), "function");
        assert_eq!(SymbolKind::Method.to_string(), "method");
        assert_eq!(SymbolKind::Struct.to_string(), "struct");
        assert_eq!(SymbolKind::Enum.to_string(), "enum");
        assert_eq!(SymbolKind::Trait.to_string(), "trait");
        assert_eq!(SymbolKind::Impl.to_string(), "impl");
        assert_eq!(SymbolKind::Mod.to_string(), "mod");
        assert_eq!(SymbolKind::Macro.to_string(), "macro");
        assert_eq!(SymbolKind::Const.to_string(), "const");
        assert_eq!(SymbolKind::TypeAlias.to_string(), "type_alias");
        assert_eq!(SymbolKind::Class.to_string(), "class");
    }

    #[test]
    fn test_symbol_kind_from_str() {
        use std::str::FromStr;
        assert_eq!(
            SymbolKind::from_str("function").unwrap(),
            SymbolKind::Function
        );
        assert_eq!(SymbolKind::from_str("Method").unwrap(), SymbolKind::Method);
        assert_eq!(SymbolKind::from_str("STRUCT").unwrap(), SymbolKind::Struct);
        assert_eq!(
            SymbolKind::from_str("type_alias").unwrap(),
            SymbolKind::TypeAlias
        );
        assert_eq!(
            SymbolKind::from_str("typealias").unwrap(),
            SymbolKind::TypeAlias
        );
        assert_eq!(SymbolKind::from_str("class").unwrap(), SymbolKind::Class);
        assert!(SymbolKind::from_str("unknown_kind").is_err());
    }

    #[test]
    fn test_make_symbol_id_format() {
        let id = make_symbol_id(Path::new("src/foo.rs"), "Foo", &SymbolKind::Struct);
        assert!(id.ends_with("#struct"), "id={id}");
        assert!(id.contains("Foo"), "id={id}");

        let id2 = make_symbol_id(Path::new("src/bar.rs"), "Bar::baz", &SymbolKind::Method);
        assert!(id2.ends_with("#method"), "id2={id2}");
        assert!(id2.contains("Bar::baz"), "id2={id2}");
    }

    #[test]
    fn test_make_symbol_id_uniqueness() {
        let path = Path::new("src/lib.rs");
        let id1 = make_symbol_id(path, "Foo", &SymbolKind::Struct);
        let id2 = make_symbol_id(path, "Foo", &SymbolKind::Function);
        let id3 = make_symbol_id(path, "Bar", &SymbolKind::Struct);
        assert_ne!(id1, id2);
        assert_ne!(id1, id3);
    }
}
