use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct SwiftParser;

impl LanguageParser for SwiftParser {
    fn language(&self) -> Language {
        Language::Swift
    }

    fn extensions(&self) -> &[&str] {
        &["swift"]
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

/// Walk backwards through preceding siblings collecting `//` or `///` line comments.
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

/// Returns the kind string of the first anonymous child of `node`.
/// Used to distinguish `class` / `struct` / `enum` / `extension` / `actor`
/// declarations, which all share the `class_declaration` node kind in tree-sitter-swift.
fn first_keyword(node: Node) -> &'static str {
    // tree-sitter keyword strings are interned static strs from the grammar tables.
    // Casting via pointer is safe because the grammar outlives any parsed tree.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !child.is_named() {
            // SAFETY: kind() returns a pointer into the static grammar tables.
            return unsafe { &*(child.kind() as *const str) };
        }
    }
    ""
}

/// Returns `(byte_end, line_end)` for a Swift class/struct/actor node.
///
/// Trims to the header line when the body contains method or init declarations,
/// leaving only the declaration line visible via `get_symbol`.
/// Enums, protocols, and extensions are returned at full extent.
fn type_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    // Only trim class / struct / actor bodies.
    if !matches!(first_keyword(node), "class" | "struct" | "actor") {
        return full;
    }

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_methods = {
        let mut cursor = body.walk();
        let result = body
            .children(&mut cursor)
            .any(|c| matches!(c.kind(), "function_declaration" | "init_declaration"));
        result
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
        language: Language::Swift,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end,
        line_start: start_pos.row as u32 + 1,
        line_end,
        signature,
        doc,
    });
}

fn push_function_symbol(
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
        language: Language::Swift,
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
    type_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "class_declaration" => {
            match first_keyword(node) {
                "class" | "actor" => {
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
                "struct" => {
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
                "enum" => {
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
                "extension" => {
                    // Extensions add methods to an existing type; use the extended
                    // type name as the qualifier so methods appear as `Type::method`.
                    let ext_name = node
                        .child_by_field_name("name")
                        .map(|n| node_text(source, n).to_string());
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut cursor = body.walk();
                        for child in body.children(&mut cursor) {
                            extract_from_node(source, child, path, ext_name.as_deref(), symbols);
                        }
                    }
                }
                _ => {}
            }
        }
        "protocol_declaration" => {
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
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(source, name_node).to_string();
                let (qualified, name, kind) = match type_name {
                    Some(t) => (format!("{t}::{fn_name}"), fn_name, SymbolKind::Method),
                    None => (fn_name.clone(), fn_name, SymbolKind::Function),
                };
                push_function_symbol(source, node, path, name, qualified, kind, symbols);
            }
        }
        "protocol_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(source, name_node).to_string();
                let (qualified, name) = match type_name {
                    Some(t) => (format!("{t}::{fn_name}"), fn_name),
                    None => (fn_name.clone(), fn_name),
                };
                push_function_symbol(
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
        "init_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let init_name = node_text(source, name_node).to_string();
                let (qualified, name) = match type_name {
                    Some(t) => (format!("{t}::{init_name}"), init_name),
                    None => (init_name.clone(), init_name),
                };
                push_function_symbol(
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
        "typealias_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::TypeAlias, symbols);
            }
        }
        // Don't recurse into function bodies.
        "function_body" => {}
        _ => {
            // At the top level, recurse to catch any wrapped declarations.
            if type_name.is_none() {
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
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        SwiftParser.extract_symbols(source, &tree, Path::new("test.swift"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"class Foo {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_struct() {
        let source = b"struct Point { var x: Double; var y: Double }";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(matches!(s.kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_enum() {
        let source = b"enum Direction { case north, south, east, west }";
        let symbols = parse_and_extract(source);
        let e = symbols.iter().find(|s| s.name == "Direction").unwrap();
        assert!(matches!(e.kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_protocol() {
        let source = b"protocol Runnable { func run() }";
        let symbols = parse_and_extract(source);
        let p = symbols.iter().find(|s| s.name == "Runnable").unwrap();
        assert!(matches!(p.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_method() {
        let source = b"class Greeter {\n    func greet() -> String { return \"Hi\" }\n}";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::greet");
    }

    #[test]
    fn test_extract_init() {
        let source = b"class Point {\n    init(x: Int, y: Int) {}\n}";
        let symbols = parse_and_extract(source);
        let init = symbols.iter().find(|s| s.name == "init").unwrap();
        assert!(matches!(init.kind, SymbolKind::Method));
        assert_eq!(init.qualified, "Point::init");
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = b"func greet(name: String) -> String { return name }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_extension_method() {
        let source = b"extension Greeter {\n    func shout() -> String { return \"HI\" }\n}";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "shout").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::shout");
    }

    #[test]
    fn test_extract_typealias() {
        let source = b"typealias Name = String";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Name");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_protocol_method() {
        let source = b"protocol Greetable {\n    func greet() -> String\n}";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greetable::greet");
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"/// A greeting class.\nclass Hello {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("greeting"), "doc={doc}");
    }

    #[test]
    fn test_class_body_trimmed_when_has_methods() {
        let source = b"class Service {\n    func run() {\n        doWork()\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class should be trimmed to header line"
        );
    }

    #[test]
    fn test_class_not_trimmed_when_no_methods() {
        let source = b"class Point {\n    var x: Double\n    var y: Double\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(
            cls.line_end > cls.line_start,
            "class without methods should not be trimmed"
        );
    }

    #[test]
    fn test_struct_body_trimmed_when_has_methods() {
        let source =
            b"struct Vector {\n    var x: Double\n    func length() -> Double { return x }\n}";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Vector").unwrap();
        assert_eq!(
            s.line_start, s.line_end,
            "struct with methods should be trimmed to header line"
        );
    }

    #[test]
    fn test_multiple_top_level_types() {
        let source = b"class Foo {}\nstruct Bar {}\nenum Baz { case a }";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"Baz"));
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
        let source = b"class Foo: Bar {\n    func baz() {}\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "class Foo: Bar {");
    }

    #[test]
    fn test_multiple_methods() {
        let source = b"class Engine {\n    func start() {}\n    func stop() {}\n}";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "stop").unwrap();
        assert_eq!(start.qualified, "Engine::start");
        assert_eq!(stop.qualified, "Engine::stop");
    }

    #[test]
    fn test_init_trimmed_from_class_body() {
        let source = b"class Service {\n    init() {}\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class with init should be trimmed"
        );
    }
}
