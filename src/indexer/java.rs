use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct JavaParser;

impl LanguageParser for JavaParser {
    fn language(&self) -> Language {
        Language::Java
    }

    fn extensions(&self) -> &[&str] {
        &["java"]
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

/// Walk backwards through preceding siblings collecting Javadoc (`/** */`) or
/// consecutive line comments.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut line_comments: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "block_comment" => {
                let text = node_text(source, p);
                if text.starts_with("/**") {
                    // Javadoc — takes priority over any line comments collected so far.
                    return Some(text.to_string());
                }
                break;
            }
            "line_comment" => {
                line_comments.push(node_text(source, p).to_string());
                prev = p.prev_sibling();
                continue;
            }
            _ => break,
        }
    }
    if line_comments.is_empty() {
        return None;
    }
    line_comments.reverse();
    Some(line_comments.join("\n"))
}

/// Returns `(byte_end, line_end)` for a Java class or record node.
///
/// Trims to the header line when the class body contains method or constructor
/// definitions (i.e. non-abstract, non-empty bodies), leaving only the
/// declaration line visible via `get_symbol`. Field-only or empty classes are
/// returned at full extent. Interface, enum, and annotation bodies are never
/// trimmed — their member signatures are the API surface.
fn class_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    // Only trim class/record bodies — interfaces, enums, annotations stay intact.
    if !matches!(node.kind(), "class_declaration" | "record_declaration") {
        return full;
    }

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_method_bodies = {
        let mut cursor = body.walk();
        let result = body.children(&mut cursor).any(|child| {
            matches!(
                child.kind(),
                "method_declaration" | "constructor_declaration"
            ) && child.child_by_field_name("body").is_some()
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
        language: Language::Java,
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
        language: Language::Java,
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
        "class_declaration" | "record_declaration" => {
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
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Enum, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "annotation_type_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Interface, symbols);
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
        "constructor_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let ctor_name = node_text(source, name_node).to_string();
                let (qualified, name) = match class_name {
                    Some(cls) => (format!("{cls}::{ctor_name}"), ctor_name),
                    None => (ctor_name.clone(), ctor_name),
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
        // Don't recurse into method/constructor bodies.
        "block" | "constructor_body" => {}
        _ => {
            // At the top level, recurse to catch any wrapped declarations.
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
            .set_language(&tree_sitter_java::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        JavaParser.extract_symbols(source, &tree, Path::new("test.java"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"public class Foo {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_interface() {
        let source = b"public interface Runnable { void run(); }";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "Runnable").unwrap();
        assert!(matches!(iface.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_enum() {
        let source = b"public enum Color { RED, GREEN, BLUE }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_annotation_type() {
        let source = b"public @interface Override {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Override");
        assert!(matches!(symbols[0].kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_method() {
        let source = b"public class Greeter { public void hello() {} }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "hello").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::hello");
    }

    #[test]
    fn test_extract_constructor() {
        let source = b"public class Point { public Point(int x, int y) {} }";
        let symbols = parse_and_extract(source);
        let ctor = symbols
            .iter()
            .find(|s| s.name == "Point" && matches!(s.kind, SymbolKind::Method))
            .unwrap();
        assert_eq!(ctor.qualified, "Point::Point");
    }

    #[test]
    fn test_method_qualified_name() {
        let source = b"class Engine { void start() {} void stop() {} }";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "stop").unwrap();
        assert_eq!(start.qualified, "Engine::start");
        assert_eq!(stop.qualified, "Engine::stop");
    }

    #[test]
    fn test_interface_method_qualified_name() {
        let source = b"interface Handler { void handle(String s); }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "handle").unwrap();
        assert_eq!(method.qualified, "Handler::handle");
    }

    #[test]
    fn test_javadoc_extracted() {
        let source = b"/** Greets the world. */\npublic class Hello {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Greets"), "doc={doc}");
    }

    #[test]
    fn test_line_comment_doc_extracted() {
        let source = b"// A simple counter.\nclass Counter { int count; }";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("counter"), "doc={doc}");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"public class Foo {\n    void bar() {}\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "public class Foo {");
    }

    #[test]
    fn test_class_body_trimmed_when_has_methods() {
        // A class with a method body should be trimmed to the header line only.
        let source =
            b"public class Service {\n    public void run() {\n        doWork();\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        // The symbol should end at the opening brace line, not at the closing brace.
        assert_eq!(
            cls.line_start, cls.line_end,
            "class should be trimmed to header line"
        );
    }

    #[test]
    fn test_class_not_trimmed_when_field_only() {
        // A class with only fields should show the full body.
        let source = b"public class Point {\n    int x;\n    int y;\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(
            cls.line_end > cls.line_start,
            "field-only class should not be trimmed"
        );
    }

    #[test]
    fn test_line_numbers() {
        let source = b"class First {}\n\nclass Second {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_multiple_top_level_classes() {
        let source = b"class Foo {}\nclass Bar {}";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_inner_class_extracted() {
        let source = b"class Outer { class Inner {} }";
        let symbols = parse_and_extract(source);
        assert!(symbols.iter().any(|s| s.name == "Outer"));
        assert!(symbols.iter().any(|s| s.name == "Inner"));
    }

    #[test]
    fn test_record_extracted_as_class() {
        let source = b"public record Point(int x, int y) {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Point");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }
}
