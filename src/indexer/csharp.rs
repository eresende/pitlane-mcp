use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct CSharpParser;

impl LanguageParser for CSharpParser {
    fn language(&self) -> Language {
        Language::CSharp
    }

    fn extensions(&self) -> &[&str] {
        &["cs"]
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

/// Walk backwards through preceding siblings collecting XML doc comments (`/// ...`)
/// or consecutive line/block comments.
///
/// tree-sitter-c-sharp uses a single `comment` kind for all comments;
/// `///` and `//` are both `comment` nodes.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut comments: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            comments.push(node_text(source, p).to_string());
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    if comments.is_empty() {
        return None;
    }
    comments.reverse();
    Some(comments.join("\n"))
}

/// Returns `(byte_end, line_end)` for a C# class, struct, or record node.
///
/// Trims to the header line when the body contains method or constructor
/// definitions with bodies, leaving only the declaration line visible via
/// `get_symbol`. Field-only or empty types are returned at full extent.
/// Interface and enum bodies are never trimmed.
fn type_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    // Only trim class/struct/record bodies.
    if !matches!(
        node.kind(),
        "class_declaration" | "struct_declaration" | "record_declaration"
    ) {
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
    let (byte_end, line_end) = type_symbol_end(source, node);
    symbols.push(Symbol {
        id,
        name: name.to_string(),
        qualified: name.to_string(),
        kind,
        language: Language::CSharp,
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
        language: Language::CSharp,
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
        // Recurse through namespace bodies to reach type declarations.
        "namespace_declaration" => {
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_from_node(source, child, path, None, symbols);
                }
            }
        }
        // File-scoped namespaces (C# 10+): declarations are direct children.
        "file_scoped_namespace_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named()
                    && child.kind() != "identifier"
                    && child.kind() != "qualified_name"
                {
                    extract_from_node(source, child, path, None, symbols);
                }
            }
        }
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
            }
        }
        "struct_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Struct, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "delegate_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::TypeAlias, symbols);
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
        // Don't recurse into method bodies.
        "block" => {}
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
            .set_language(&tree_sitter_c_sharp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        CSharpParser.extract_symbols(source, &tree, Path::new("test.cs"))
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
        let source = b"public interface IRunnable { void Run(); }";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "IRunnable").unwrap();
        assert!(matches!(iface.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_enum() {
        let source = b"public enum Color { Red, Green, Blue }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_struct() {
        let source = b"public struct Point { public int X; public int Y; }";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(matches!(s.kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_delegate() {
        let source = b"public delegate void EventHandler(object sender);";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "EventHandler");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_record() {
        let source = b"public record Point(int X, int Y);";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Point");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_method() {
        let source = b"public class Greeter { public void Hello() {} }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "Hello").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::Hello");
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
    fn test_namespace_declaration() {
        let source = b"namespace MyApp { public class Service {} }";
        let symbols = parse_and_extract(source);
        assert!(symbols.iter().any(|s| s.name == "Service"));
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"/// A greeting service.\npublic class Hello {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("greeting"), "doc={doc}");
    }

    #[test]
    fn test_class_body_trimmed_when_has_methods() {
        let source =
            b"public class Service {\n    public void Run() {\n        DoWork();\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class should be trimmed to header line"
        );
    }

    #[test]
    fn test_class_not_trimmed_when_field_only() {
        let source = b"public class Point {\n    public int X;\n    public int Y;\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(
            cls.line_end > cls.line_start,
            "field-only class should not be trimmed"
        );
    }

    #[test]
    fn test_method_qualified_name() {
        let source = b"class Engine { void Start() {} void Stop() {} }";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "Start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "Stop").unwrap();
        assert_eq!(start.qualified, "Engine::Start");
        assert_eq!(stop.qualified, "Engine::Stop");
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
    fn test_line_numbers() {
        let source = b"class First {}\n\nclass Second {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"public class Foo {\n    void Bar() {}\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "public class Foo {");
    }
}
