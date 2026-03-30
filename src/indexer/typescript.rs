use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct TypeScriptParser;

impl LanguageParser for TypeScriptParser {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn extensions(&self) -> &[&str] {
        &["ts", "tsx", "mts", "cts"]
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

/// Returns `(byte_end, line_end)` for a TS class node.
/// For classes with methods, trims to just the header line so that
/// `get_symbol` returns only the declaration, not the full body.
/// Classes with no methods are returned at full extent.
fn class_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_methods = {
        let mut cursor = body.walk();
        let x = body.children(&mut cursor).any(|child| {
            matches!(
                child.kind(),
                "method_definition" | "abstract_method_signature"
            )
        });
        x
    };

    if !has_methods {
        return full;
    }

    // Trim to end of the header line (the line containing the opening `{`).
    let body_start_byte = body.start_byte();
    let body_start_row = body.start_position().row;
    let after_open = &source[body_start_byte..node.end_byte()];
    for (i, &b) in after_open.iter().enumerate() {
        if b == b'\n' {
            return (body_start_byte + i + 1, body_start_row as u32 + 1);
        }
    }

    full
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
        language: Language::TypeScript,
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
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Class);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let (byte_end, line_end) = class_symbol_end(source, node);
                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name.clone(),
                    kind: SymbolKind::Class,
                    language: Language::TypeScript,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end,
                    line_start: start_pos.row as u32 + 1,
                    line_end,
                    signature,
                    doc,
                });
                if let Some(body) = node.child_by_field_name("body") {
                    extract_methods(source, body, &name, path, symbols);
                }
            }
            // Don't recurse further into class internals
        }
        "interface_declaration" => {
            if let Some(name) = get_name(source, node) {
                push_symbol(
                    symbols,
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::Interface,
                );
            }
        }
        "type_alias_declaration" => {
            if let Some(name) = get_name(source, node) {
                push_symbol(
                    symbols,
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::TypeAlias,
                );
            }
        }
        "enum_declaration" => {
            if let Some(name) = get_name(source, node) {
                push_symbol(
                    symbols,
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::Enum,
                );
            }
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
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        TypeScriptParser.extract_symbols(source, &tree, Path::new("test.ts"))
    }

    fn parse_and_extract_tsx(source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        TypeScriptParser.extract_symbols(source, &tree, Path::new("test.tsx"))
    }

    #[test]
    fn test_function_declaration() {
        let symbols = parse_and_extract(b"function hello(): void {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
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
        let source = b"class Dog {\n  bark(): void {}\n  fetch(): void {}\n}";
        let symbols = parse_and_extract(source);

        let class = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert_eq!(class.name, "Dog");

        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 2);
        assert_eq!(
            methods.iter().find(|s| s.name == "bark").unwrap().qualified,
            "Dog.bark"
        );
    }

    #[test]
    fn test_abstract_class() {
        let symbols = parse_and_extract(b"abstract class Shape {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Shape");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_interface_declaration() {
        let symbols = parse_and_extract(b"interface Greetable { greet(): void; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Greetable");
        assert!(matches!(symbols[0].kind, SymbolKind::Interface));
    }

    #[test]
    fn test_type_alias() {
        let symbols = parse_and_extract(b"type ID = string | number;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "ID");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_enum_declaration() {
        let symbols = parse_and_extract(b"enum Direction { Up, Down, Left, Right }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Direction");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_exported_interface() {
        let symbols = parse_and_extract(b"export interface Config { port: number; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Config");
        assert!(matches!(symbols[0].kind, SymbolKind::Interface));
    }

    #[test]
    fn test_exported_type_alias() {
        let symbols = parse_and_extract(b"export type Result<T> = T | Error;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Result");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_tsx_function_component() {
        let source = b"function App() { return <div />; }";
        let symbols = parse_and_extract_tsx(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "App");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_no_nested_functions() {
        let source = b"function outer(): void {\n  function inner(): void {}\n}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "outer");
    }

    #[test]
    fn test_multiple_ts_declarations() {
        let source = b"interface Foo {}\ntype Bar = string;\nenum Baz { A }\nfunction qux() {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 4);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"Baz"));
        assert!(names.contains(&"qux"));
    }

    #[test]
    fn test_jsdoc_attached() {
        let source = b"/** A greeter. */\nfunction greet(): void {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("A greeter"), "doc={doc}");
    }

    #[test]
    fn test_language_is_typescript() {
        let symbols = parse_and_extract(b"function f() {}");
        assert_eq!(symbols[0].language, Language::TypeScript);
    }

    #[test]
    fn test_const_arrow_function() {
        let symbols = parse_and_extract(b"const greet = (): void => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_async_arrow_function() {
        let symbols = parse_and_extract(b"const fetchData = async (): Promise<void> => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "fetchData");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_arrow_with_type_annotation() {
        let symbols = parse_and_extract(b"const handler: (req: Request) => void = (req) => {};");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "handler");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_export_const_arrow_function() {
        let symbols = parse_and_extract(b"export const render = (): string => '';");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "render");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_const_non_function_not_extracted() {
        let symbols = parse_and_extract(b"const MAX_RETRIES = 3;");
        assert_eq!(symbols.len(), 0);
    }

    #[test]
    fn test_tsx_const_arrow_component() {
        let source = b"const Button = (): JSX.Element => <button />;";
        let symbols = parse_and_extract_tsx(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Button");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    /// A class with methods should have its symbol trimmed to just the header line.
    #[test]
    fn test_class_with_methods_trimmed_to_header() {
        let source = b"class Dog {\n  bark(): void {}\n  fetch(): void {}\n}";
        let symbols = parse_and_extract(source);
        let class_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert_eq!(class_sym.line_start, 1);
        assert_eq!(
            class_sym.line_end, 1,
            "class symbol should be trimmed to header only"
        );
        let source_str =
            std::str::from_utf8(&source[class_sym.byte_start..class_sym.byte_end]).unwrap();
        assert!(source_str.starts_with("class Dog {"), "got: {source_str:?}");
        assert!(!source_str.contains("bark"), "body should not be included");
    }

    /// An abstract class with methods should also be trimmed.
    #[test]
    fn test_abstract_class_with_methods_trimmed() {
        let source = b"abstract class Shape {\n  abstract area(): number;\n}";
        let symbols = parse_and_extract(source);
        let class_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert_eq!(
            class_sym.line_end, 1,
            "abstract class should be trimmed to header only"
        );
        let source_str =
            std::str::from_utf8(&source[class_sym.byte_start..class_sym.byte_end]).unwrap();
        assert!(!source_str.contains("area"), "body should not be included");
    }

    /// A class without methods (empty or field-only) should NOT be trimmed.
    #[test]
    fn test_empty_class_not_trimmed() {
        let source = b"class Config {}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let class_sym = &symbols[0];
        let source_str =
            std::str::from_utf8(&source[class_sym.byte_start..class_sym.byte_end]).unwrap();
        assert!(source_str.contains("Config"), "got: {source_str:?}");
    }
}
