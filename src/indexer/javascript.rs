use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct JavaScriptParser;

impl LanguageParser for JavaScriptParser {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn extensions(&self) -> &[&str] {
        &["js", "jsx", "mjs", "cjs"]
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

/// Returns the immediately preceding JSDoc block comment (`/** ... */`), if any.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    let mut prev_nodes: Vec<Node> = Vec::new();

    for child in parent.children(&mut cursor) {
        if child.id() == node.id() {
            break;
        }
        prev_nodes.push(child);
    }

    for sibling in prev_nodes.iter().rev() {
        if sibling.kind() == "comment" {
            let text = node_text(source, *sibling);
            if text.starts_with("/**") {
                return Some(text.to_string());
            }
            break;
        } else if !sibling.is_extra() {
            break;
        }
    }

    None
}

fn push_symbol(
    symbols: &mut Vec<Symbol>,
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    name: String,
    qualified: String,
    kind: SymbolKind,
) {
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
        language: Language::JavaScript,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

fn extract_methods(
    source: &[u8],
    class_body: Node,
    class_name: &str,
    path: &std::path::Path,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = class_body.walk();
    for child in class_body.children(&mut cursor) {
        if child.kind() == "method_definition" {
            if let Some(name) = get_name(source, child) {
                let qualified = format!("{}.{}", class_name, name);
                push_symbol(
                    symbols,
                    source,
                    child,
                    path,
                    name,
                    qualified,
                    SymbolKind::Method,
                );
            }
        }
    }
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    class_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name) = get_name(source, node) {
                let (kind, qualified) = if let Some(cls) = class_name {
                    (SymbolKind::Method, format!("{}.{}", cls, name))
                } else {
                    (SymbolKind::Function, name.clone())
                };
                push_symbol(symbols, source, node, path, name, qualified, kind);
            }
            // Don't recurse into function body
        }
        "class_declaration" => {
            if let Some(name) = get_name(source, node) {
                push_symbol(
                    symbols,
                    source,
                    node,
                    path,
                    name.clone(),
                    name.clone(),
                    SymbolKind::Class,
                );
                if let Some(body) = node.child_by_field_name("body") {
                    extract_methods(source, body, &name, path, symbols);
                }
            }
            // Don't recurse further into class internals
        }
        "lexical_declaration" | "variable_declaration" => {
            // Handle `const foo = () => {}` and `const foo = function() {}`
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    let value_kind = child
                        .child_by_field_name("value")
                        .map(|v| v.kind())
                        .unwrap_or("");
                    if matches!(value_kind, "arrow_function" | "function_expression") {
                        if let Some(name) = get_name(source, child) {
                            // Use the lexical_declaration node for position/doc/signature so
                            // that the `const` keyword and any preceding JSDoc are included.
                            push_symbol(
                                symbols,
                                source,
                                node,
                                path,
                                name.clone(),
                                name,
                                SymbolKind::Function,
                            );
                        }
                    }
                }
            }
        }
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // Don't descend into function or class bodies
                if !matches!(child.kind(), "statement_block" | "class_body") {
                    extract_from_node(source, child, path, class_name, symbols);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_and_extract(source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_javascript::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        JavaScriptParser.extract_symbols(source, &tree, Path::new("test.js"))
    }

    #[test]
    fn test_function_declaration() {
        let symbols = parse_and_extract(b"function hello() {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_generator_function() {
        let symbols = parse_and_extract(b"function* gen() {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "gen");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_class_declaration() {
        let symbols = parse_and_extract(b"class Foo {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_class_with_methods() {
        let source = b"class Animal {\n  constructor(name) {}\n  speak() {}\n}";
        let symbols = parse_and_extract(source);

        let class = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert_eq!(class.name, "Animal");

        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        let names: Vec<_> = methods.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"constructor"));
        assert!(names.contains(&"speak"));
        assert_eq!(
            methods
                .iter()
                .find(|s| s.name == "speak")
                .unwrap()
                .qualified,
            "Animal.speak"
        );
    }

    #[test]
    fn test_exported_function() {
        let symbols = parse_and_extract(b"export function greet() {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_exported_class() {
        let symbols = parse_and_extract(b"export class MyClass {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyClass");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_no_nested_functions() {
        let source = b"function outer() {\n  function inner() {}\n}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "outer");
    }

    #[test]
    fn test_multiple_declarations() {
        let source = b"function foo() {}\nclass Bar {}\nfunction baz() {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 3);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"baz"));
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"function complex(\n  arg1,\n  arg2\n) {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "function complex(");
    }

    #[test]
    fn test_jsdoc_attached() {
        let source = b"/** Does a thing. */\nfunction doThing() {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Does a thing"), "doc={doc}");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"function first() {}\nfunction second() {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert_eq!(first.line_start, 1);
        assert_eq!(second.line_start, 2);
    }

    #[test]
    fn test_const_arrow_function() {
        let symbols = parse_and_extract(b"const greet = () => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_arrow_with_params() {
        let symbols = parse_and_extract(b"const add = (a, b) => a + b;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "add");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_async_arrow_function() {
        let symbols = parse_and_extract(b"const fetchData = async () => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "fetchData");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_function_expression() {
        let symbols = parse_and_extract(b"const handler = function() {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "handler");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_export_const_arrow_function() {
        let symbols = parse_and_extract(b"export const render = () => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "render");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_non_function_not_extracted() {
        // Plain value assignments should not be extracted
        let symbols = parse_and_extract(b"const FOO = 42;");
        assert_eq!(symbols.len(), 0);
    }

    #[test]
    fn test_arrow_signature_includes_const() {
        let symbols = parse_and_extract(b"const greet = () => {};");
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert!(sig.starts_with("const greet"), "sig={sig}");
    }

    #[test]
    fn test_jsdoc_on_const_arrow() {
        let source = b"/** Handles requests. */\nconst handler = async (req) => {};";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Handles requests"), "doc={doc}");
    }
}
