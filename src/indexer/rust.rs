use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct RustParser;

impl LanguageParser for RustParser {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_from_node(source, root, path, None, &mut symbols);
        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    // Look at preceding siblings for doc comments
    let mut docs = Vec::new();
    let parent = node.parent()?;
    let mut cursor = parent.walk();

    let mut prev_was_doc = false;
    let mut prev_nodes: Vec<Node> = Vec::new();

    for child in parent.children(&mut cursor) {
        if child.id() == node.id() {
            break;
        }
        prev_nodes.push(child);
    }

    // Walk backwards through preceding siblings collecting doc comments
    let mut collecting = true;
    for sibling in prev_nodes.iter().rev() {
        if !collecting {
            break;
        }
        let kind = sibling.kind();
        if kind == "line_comment" || kind == "block_comment" {
            let text = node_text(source, *sibling);
            if text.starts_with("///") || text.starts_with("/**") {
                docs.push(text.to_string());
                prev_was_doc = true;
            } else {
                collecting = false;
            }
        } else if kind != "\n" && !sibling.is_extra() {
            // Non-comment node: stop collecting if we already have docs
            if prev_was_doc {
                break;
            }
            collecting = false;
        }
    }

    if docs.is_empty() {
        return None;
    }

    docs.reverse();
    Some(docs.join("\n"))
}

fn get_name(source: &[u8], node: Node) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(source, n).to_string())
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    let first_line = text.lines().next()?;
    Some(first_line.trim_end().to_string())
}

fn extract_impl_type_name(source: &[u8], node: Node) -> String {
    // Look for trait_type and type fields on impl_item
    let trait_type = node.child_by_field_name("trait");
    let self_type = node.child_by_field_name("type");

    match (trait_type, self_type) {
        (Some(tr), Some(ty)) => {
            format!(
                "impl {} for {}",
                node_text(source, tr),
                node_text(source, ty)
            )
        }
        (None, Some(ty)) => {
            format!("impl {}", node_text(source, ty))
        }
        _ => "impl".to_string(),
    }
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    impl_type: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_item" => {
            if let Some(name) = get_name(source, node) {
                let (kind, qualified) = if let Some(impl_name) = impl_type {
                    (
                        SymbolKind::Method,
                        format!(
                            "{}::{}",
                            impl_name
                                .trim_start_matches("impl ")
                                .split(" for ")
                                .last()
                                .unwrap_or(impl_name),
                            name
                        ),
                    )
                } else {
                    (SymbolKind::Function, name.clone())
                };

                let id = make_symbol_id(path, &qualified, &kind);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name,
                    qualified,
                    kind,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into function bodies
        }
        "struct_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Struct);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Struct,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "enum_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Enum);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Enum,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "trait_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Trait);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Trait,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into trait body (would need to handle trait methods differently)
        }
        "impl_item" => {
            let impl_name = extract_impl_type_name(source, node);
            let qualified = impl_name.clone();
            let id = make_symbol_id(path, &qualified, &SymbolKind::Impl);
            let doc = get_doc_comment(source, node);
            let signature = get_signature(source, node);
            let start_pos = node.start_position();
            let end_pos = node.end_position();

            // Extract a short name for the impl (just the type name)
            let name = node
                .child_by_field_name("type")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_else(|| "impl".to_string());

            symbols.push(Symbol {
                id,
                name,
                qualified,
                kind: SymbolKind::Impl,
                language: Language::Rust,
                file: Arc::new(path.to_path_buf()),
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                line_start: start_pos.row as u32 + 1,
                line_end: end_pos.row as u32 + 1,
                signature,
                doc,
            });

            // Extract the type name for method qualification
            let type_name = node
                .child_by_field_name("type")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_else(|| "Unknown".to_string());

            // Recurse into impl body to find methods
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "declaration_list" {
                    let mut inner_cursor = child.walk();
                    for inner_child in child.children(&mut inner_cursor) {
                        if inner_child.kind() == "function_item" {
                            extract_from_node(source, inner_child, path, Some(&type_name), symbols);
                        }
                    }
                }
            }
        }
        "mod_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Mod);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Mod,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
                // Don't recurse into mod body
            }
        }
        "macro_rules" => {
            if let Some(name) = node
                .child_by_field_name("name")
                .map(|n| node_text(source, n).to_string())
                .or_else(|| {
                    // Sometimes macro name is in a different position
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "identifier" {
                            return Some(node_text(source, child).to_string());
                        }
                    }
                    None
                })
            {
                let id = make_symbol_id(path, &name, &SymbolKind::Macro);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Macro,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "const_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Const);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Const,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "type_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::TypeAlias);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::TypeAlias,
                    language: Language::Rust,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        _ => {
            // Recurse into other nodes at top level
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // Don't recurse into function bodies / blocks
                if child.kind() != "block" {
                    extract_from_node(source, child, path, impl_type, symbols);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::SymbolKind;
    use std::path::Path;

    fn parse_and_extract(source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        RustParser.extract_symbols(source, &tree, Path::new("test.rs"))
    }

    #[test]
    fn test_extract_function() {
        let symbols = parse_and_extract(b"fn hello() {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_pub_function() {
        let symbols = parse_and_extract(b"pub fn greet(name: &str) -> String { String::new() }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_struct() {
        let symbols = parse_and_extract(b"pub struct Foo { x: i32 }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_enum() {
        let symbols = parse_and_extract(b"enum Color { Red, Green, Blue }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_trait() {
        let symbols = parse_and_extract(b"pub trait Greet { fn greet(&self); }");
        let trait_syms: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Trait))
            .collect();
        assert_eq!(trait_syms.len(), 1);
        assert_eq!(trait_syms[0].name, "Greet");
    }

    #[test]
    fn test_extract_impl_and_methods() {
        let source = b"struct Point { x: f64 }\nimpl Point {\n    pub fn new(x: f64) -> Self { Point { x } }\n}";
        let symbols = parse_and_extract(source);

        let impl_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Impl))
            .unwrap();
        assert!(impl_sym.qualified.contains("Point"));

        let method = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Method))
            .unwrap();
        assert_eq!(method.name, "new");
        assert_eq!(method.qualified, "Point::new");
    }

    #[test]
    fn test_extract_trait_impl() {
        let source = b"trait Greet {}\nstruct Foo;\nimpl Greet for Foo {}";
        let symbols = parse_and_extract(source);
        let impl_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Impl))
            .unwrap();
        assert!(
            impl_sym.qualified.contains("Greet"),
            "qualified={}",
            impl_sym.qualified
        );
        assert!(
            impl_sym.qualified.contains("Foo"),
            "qualified={}",
            impl_sym.qualified
        );
    }

    #[test]
    fn test_extract_mod() {
        let symbols = parse_and_extract(b"mod my_module;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "my_module");
        assert!(matches!(symbols[0].kind, SymbolKind::Mod));
    }

    #[test]
    fn test_extract_type_alias() {
        let symbols = parse_and_extract(b"pub type MyResult<T> = std::result::Result<T, String>;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyResult");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_const() {
        let symbols = parse_and_extract(b"const MAX: u32 = 100;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MAX");
        assert!(matches!(symbols[0].kind, SymbolKind::Const));
    }

    #[test]
    fn test_doc_comment_attached_to_symbol() {
        let source = b"/// Adds two numbers.\nfn add(a: i32, b: i32) -> i32 { a + b }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Adds two numbers"), "doc={doc}");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source =
            b"pub fn complex(\n    arg1: i32,\n    arg2: i32,\n) -> i32 {\n    arg1 + arg2\n}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "pub fn complex(");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"fn first() {}\nfn second() {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert_eq!(first.line_start, 1);
        assert_eq!(second.line_start, 2);
    }

    #[test]
    fn test_multiple_items() {
        let source = b"struct A;\nstruct B;\nfn c() {}\nenum D {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 4);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"c"));
        assert!(names.contains(&"D"));
    }
}
