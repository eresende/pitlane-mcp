use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct PhpParser;

impl LanguageParser for PhpParser {
    fn language(&self) -> Language {
        Language::Php
    }

    fn extensions(&self) -> &[&str] {
        &["php"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_from_node(source, child, path, None, &mut symbols);
        }
        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    Some(text.lines().next()?.trim_end().to_string())
}

/// Walk backwards through preceding siblings collecting PHPDoc (`/** */`) or
/// consecutive line comments (`//` or `#`).
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut line_comments: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(source, p);
            if text.starts_with("/**") {
                // PHPDoc block — takes priority over any line comments collected so far.
                return Some(text.to_string());
            }
            if text.starts_with("//") || text.starts_with('#') {
                line_comments.push(text.to_string());
                prev = p.prev_sibling();
                continue;
            }
            break;
        } else {
            break;
        }
    }
    if line_comments.is_empty() {
        return None;
    }
    line_comments.reverse();
    Some(line_comments.join("\n"))
}

/// Returns `(byte_end, line_end)` for a PHP class node.
///
/// Trims to the header line when the body contains method declarations with
/// bodies, leaving only the declaration line visible via `get_symbol`.
/// Interface, trait, and enum bodies are never trimmed.
fn class_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    if node.kind() != "class_declaration" {
        return full;
    }

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_method_bodies = {
        let mut cursor = body.walk();
        let result = body.children(&mut cursor).any(|child| {
            child.kind() == "method_declaration" && child.child_by_field_name("body").is_some()
        });
        result
    };

    if !has_method_bodies {
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

fn push_type_symbol(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    name: &str,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let id = make_symbol_id(path, name, &kind);
    let doc = get_doc_comment(source, node);
    let signature = get_signature(source, node);
    let start_pos = node.start_position();
    let (byte_end, line_end) = class_symbol_end(source, node);
    symbols.push(Symbol {
        id,
        name: name.to_string(),
        qualified: name.to_string(),
        kind,
        language: Language::Php,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end,
        line_start: start_pos.row as u32 + 1,
        line_end,
        signature,
        doc,
    });
}

fn push_method_symbol(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    name: String,
    qualified: String,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
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
        language: Language::Php,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    class_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Class, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Interface, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "trait_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Trait, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Enum, symbols);
            }
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(source, name_node).to_string();
                push_method_symbol(
                    source,
                    node,
                    path,
                    fn_name.clone(),
                    fn_name,
                    SymbolKind::Function,
                    symbols,
                );
            }
        }
        "method_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let method_name = node_text(source, name_node).to_string();
                let (qualified, name) = match class_name {
                    Some(cls) => (format!("{cls}::{method_name}"), method_name),
                    None => (method_name.clone(), method_name),
                };
                push_method_symbol(
                    source,
                    node,
                    path,
                    name,
                    qualified,
                    SymbolKind::Method,
                    symbols,
                );
            }
        }
        // Don't recurse into function/method bodies.
        "compound_statement" => {}
        _ => {
            if class_name.is_none() {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    extract_from_node(source, child, path, None, symbols);
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
            .set_language(&tree_sitter_php::LANGUAGE_PHP.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        PhpParser.extract_symbols(source, &tree, Path::new("test.php"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"<?php\nclass Foo {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_interface() {
        let source = b"<?php\ninterface Runnable { public function run(); }";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "Runnable").unwrap();
        assert!(matches!(iface.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_trait() {
        let source = b"<?php\ntrait Greetable { public function greet() {} }";
        let symbols = parse_and_extract(source);
        let t = symbols.iter().find(|s| s.name == "Greetable").unwrap();
        assert!(matches!(t.kind, SymbolKind::Trait));
    }

    #[test]
    fn test_extract_enum() {
        let source = b"<?php\nenum Color { case Red; case Green; case Blue; }";
        let symbols = parse_and_extract(source);
        let e = symbols.iter().find(|s| s.name == "Color").unwrap();
        assert!(matches!(e.kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = b"<?php\nfunction greet(string $name): string { return 'Hello'; }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_method() {
        let source = b"<?php\nclass Greeter { public function hello() {} }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "hello").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::hello");
    }

    #[test]
    fn test_method_qualified_name() {
        let source =
            b"<?php\nclass Engine { public function start() {} public function stop() {} }";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "stop").unwrap();
        assert_eq!(start.qualified, "Engine::start");
        assert_eq!(stop.qualified, "Engine::stop");
    }

    #[test]
    fn test_phpdoc_extracted() {
        let source = b"<?php\n/** Greets the world. */\nclass Hello {}";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Greets"), "doc={doc}");
    }

    #[test]
    fn test_line_comment_doc_extracted() {
        let source = b"<?php\n// A simple counter.\nclass Counter {}";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("counter"), "doc={doc}");
    }

    #[test]
    fn test_class_body_trimmed_when_has_methods() {
        let source =
            b"<?php\nclass Service {\n    public function run() {\n        // do work\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class should be trimmed to header line"
        );
    }

    #[test]
    fn test_class_not_trimmed_when_no_methods() {
        let source = b"<?php\nclass Point {\n    public int $x;\n    public int $y;\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(
            cls.line_end > cls.line_start,
            "property-only class should not be trimmed"
        );
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"<?php\nclass Foo extends Bar {\n    public function baz() {}\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "class Foo extends Bar {");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"<?php\nclass First {}\n\nclass Second {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_multiple_top_level_classes() {
        let source = b"<?php\nclass Foo {}\nclass Bar {}";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_abstract_class_method() {
        let source = b"<?php\nabstract class Base {\n    abstract public function run(): void;\n}";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "run").unwrap();
        assert_eq!(method.qualified, "Base::run");
    }

    #[test]
    fn test_interface_method_qualified_name() {
        let source = b"<?php\ninterface Handler { public function handle(string $s): void; }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "handle").unwrap();
        assert_eq!(method.qualified, "Handler::handle");
    }

    #[test]
    fn test_trait_method_qualified_name() {
        let source = b"<?php\ntrait Loggable { public function log(string $msg): void {} }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "log").unwrap();
        assert_eq!(method.qualified, "Loggable::log");
    }
}
