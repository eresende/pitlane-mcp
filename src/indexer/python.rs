use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct PythonParser;

impl LanguageParser for PythonParser {
    fn language(&self) -> Language {
        Language::Python
    }

    fn extensions(&self) -> &[&str] {
        &["py"]
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

fn get_name(source: &[u8], node: Node) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(source, n).to_string())
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    let first_line = text.lines().next()?;
    Some(first_line.trim_end().to_string())
}

fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    // In Python, doc strings are expression statements with string literals
    // Look at the first statement of the body
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    if let Some(child) = body.children(&mut cursor).next() {
        if child.kind() == "expression_statement" {
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                if inner.kind() == "string" {
                    return Some(node_text(source, inner).to_string());
                }
            }
        }
        // Only check first statement
    }
    None
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    class_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name) = get_name(source, node) {
                let (kind, qualified) = if let Some(cls) = class_name {
                    (SymbolKind::Method, format!("{}::{}", cls, name))
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
                    language: Language::Python,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into function body (nested functions not extracted)
        }
        "class_definition" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Class);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name.clone(),
                    kind: SymbolKind::Class,
                    language: Language::Python,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });

                // Recurse into class body to find methods
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        if child.kind() == "function_definition" {
                            extract_from_node(source, child, path, Some(&name), symbols);
                        } else if child.kind() == "decorated_definition" {
                            // Handle decorated methods
                            let mut inner_cursor = child.walk();
                            for inner in child.children(&mut inner_cursor) {
                                if inner.kind() == "function_definition" {
                                    extract_from_node(source, inner, path, Some(&name), symbols);
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // Recurse into top-level nodes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "block" {
                    extract_from_node(source, child, path, class_name, symbols);
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
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        PythonParser.extract_symbols(source, &tree, Path::new("test.py"))
    }

    #[test]
    fn test_extract_function() {
        let symbols = parse_and_extract(b"def hello():\n    pass\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_class() {
        let symbols = parse_and_extract(b"class MyClass:\n    pass\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyClass");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_class_methods() {
        let source = b"class Greeter:\n    def hello(self):\n        pass\n    def bye(self):\n        pass\n";
        let symbols = parse_and_extract(source);

        let class_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert_eq!(class_sym.name, "Greeter");

        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert!(methods.iter().any(|m| m.qualified == "Greeter::hello"));
        assert!(methods.iter().any(|m| m.qualified == "Greeter::bye"));
    }

    #[test]
    fn test_extract_function_docstring() {
        let source = b"def documented():\n    \"\"\"This is a docstring.\"\"\"\n    pass\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("docstring"), "doc={doc}");
    }

    #[test]
    fn test_extract_multiple_top_level_functions() {
        let source = b"def foo():\n    pass\n\ndef bar():\n    pass\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn test_line_numbers() {
        let source = b"def first():\n    pass\n\ndef second():\n    pass\n";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert_eq!(first.line_start, 1);
        assert!(second.line_start > first.line_start);
    }

    #[test]
    fn test_decorated_method_is_extracted() {
        let source = b"class Foo:\n    @staticmethod\n    def bar():\n        pass\n";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "bar");
        assert!(method.is_some(), "decorated method should be extracted");
        assert_eq!(method.unwrap().qualified, "Foo::bar");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"def greet(name: str) -> str:\n    return name\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "def greet(name: str) -> str:");
    }
}
