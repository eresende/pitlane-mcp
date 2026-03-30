use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct GoParser;

impl LanguageParser for GoParser {
    fn language(&self) -> Language {
        Language::Go
    }

    fn extensions(&self) -> &[&str] {
        &["go"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_top_level(source, child, path, &mut symbols);
        }
        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    let first_line = text.lines().next()?;
    Some(first_line.trim_end().to_string())
}

/// Walk backwards through preceding siblings collecting consecutive comment nodes.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut comments = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() != "comment" {
            break;
        }
        comments.push(node_text(source, p).to_string());
        prev = p.prev_sibling();
    }
    if comments.is_empty() {
        return None;
    }
    comments.reverse();
    Some(comments.join("\n"))
}

/// Extract the receiver type name from a method receiver parameter list.
/// Handles both value receivers `(r Router)` and pointer receivers `(r *Router)`.
fn get_receiver_type(source: &[u8], receiver: Node) -> Option<String> {
    let mut cursor = receiver.walk();
    for child in receiver.children(&mut cursor) {
        if child.kind() == "parameter_declaration" {
            if let Some(type_node) = child.child_by_field_name("type") {
                return match type_node.kind() {
                    "type_identifier" => Some(node_text(source, type_node).to_string()),
                    "pointer_type" => {
                        let mut pt_cursor = type_node.walk();
                        for pt_child in type_node.children(&mut pt_cursor) {
                            if pt_child.kind() == "type_identifier" {
                                return Some(node_text(source, pt_child).to_string());
                            }
                        }
                        None
                    }
                    _ => None,
                };
            }
        }
    }
    None
}

fn extract_top_level(source: &[u8], node: Node, path: &std::path::Path, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = node_text(source, name_node).to_string();
            let id = make_symbol_id(path, &name, &SymbolKind::Function);
            let doc = get_doc_comment(source, node);
            let signature = get_signature(source, node);
            let start_pos = node.start_position();
            let end_pos = node.end_position();
            symbols.push(Symbol {
                id,
                name: name.clone(),
                qualified: name,
                kind: SymbolKind::Function,
                language: Language::Go,
                file: Arc::new(path.to_path_buf()),
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                line_start: start_pos.row as u32 + 1,
                line_end: end_pos.row as u32 + 1,
                signature,
                doc,
            });
        }
        "method_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let method_name = node_text(source, name_node).to_string();
            let recv_type = node
                .child_by_field_name("receiver")
                .and_then(|r| get_receiver_type(source, r));
            let (qualified, name) = match recv_type {
                Some(ref t) => (format!("{}::{}", t, method_name), method_name.clone()),
                None => (method_name.clone(), method_name.clone()),
            };
            let id = make_symbol_id(path, &qualified, &SymbolKind::Method);
            let doc = get_doc_comment(source, node);
            let signature = get_signature(source, node);
            let start_pos = node.start_position();
            let end_pos = node.end_position();
            symbols.push(Symbol {
                id,
                name,
                qualified,
                kind: SymbolKind::Method,
                language: Language::Go,
                file: Arc::new(path.to_path_buf()),
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                line_start: start_pos.row as u32 + 1,
                line_end: end_pos.row as u32 + 1,
                signature,
                doc,
            });
        }
        "type_declaration" => {
            let doc = get_doc_comment(source, node);
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "type_spec" {
                    extract_type_spec(source, child, doc.clone(), path, symbols);
                }
            }
        }
        _ => {} // package clause, imports, var/const, comments — skip
    }
}

fn extract_type_spec(
    source: &[u8],
    node: Node,
    doc: Option<String>,
    path: &std::path::Path,
    symbols: &mut Vec<Symbol>,
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let Some(type_node) = node.child_by_field_name("type") else {
        return;
    };
    let name = node_text(source, name_node).to_string();
    let kind = match type_node.kind() {
        "struct_type" => SymbolKind::Struct,
        "interface_type" => SymbolKind::Interface,
        _ => SymbolKind::TypeAlias,
    };
    let id = make_symbol_id(path, &name, &kind);
    // Prepend "type " so the signature reads like Go source.
    let signature = get_signature(source, node).map(|s| format!("type {s}"));
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    symbols.push(Symbol {
        id,
        name: name.clone(),
        qualified: name,
        kind,
        language: Language::Go,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::SymbolKind;
    use std::path::Path;

    fn parse_and_extract(source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        GoParser.extract_symbols(source, &tree, Path::new("test.go"))
    }

    #[test]
    fn test_extract_function() {
        let source = b"package main\n\nfunc Hello() {}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_method_value_receiver() {
        let source = b"package main\n\ntype Router struct{}\n\nfunc (r Router) Handle() {}\n";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "Handle").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Router::Handle");
    }

    #[test]
    fn test_extract_method_pointer_receiver() {
        let source = b"package main\n\ntype Engine struct{}\n\nfunc (e *Engine) Run() {}\n";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "Run").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Engine::Run");
    }

    #[test]
    fn test_extract_struct() {
        let source = b"package main\n\ntype Server struct {\n\tport int\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Server");
        assert!(matches!(symbols[0].kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_interface() {
        let source = b"package main\n\ntype Handler interface {\n\tServeHTTP()\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Handler");
        assert!(matches!(symbols[0].kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_type_alias() {
        let source = b"package main\n\ntype MyInt int\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyInt");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_multiple_top_level_functions() {
        let source = b"package main\n\nfunc Foo() {}\n\nfunc Bar() {}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"package main\n\n// Hello greets the world.\nfunc Hello() {}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("greets"), "doc={doc}");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"package main\n\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "func Add(a, b int) int {");
    }

    #[test]
    fn test_struct_signature_has_type_prefix() {
        let source = b"package main\n\ntype Point struct {\n\tX, Y float64\n}\n";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Point").unwrap();
        let sig = s.signature.as_deref().unwrap_or("");
        assert!(sig.starts_with("type Point"), "sig={sig}");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"package main\n\nfunc First() {}\n\nfunc Second() {}\n";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_grouped_type_declaration() {
        let source = b"package main\n\ntype (\n\tFoo struct{}\n\tBar interface{}\n)\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        assert!(symbols
            .iter()
            .any(|s| s.name == "Foo" && matches!(s.kind, SymbolKind::Struct)));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Bar" && matches!(s.kind, SymbolKind::Interface)));
    }
}
