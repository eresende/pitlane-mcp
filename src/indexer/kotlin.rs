use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct KotlinParser;

impl LanguageParser for KotlinParser {
    fn language(&self) -> Language {
        Language::Kotlin
    }

    fn extensions(&self) -> &[&str] {
        &["kt", "kts"]
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

/// Walk backwards through preceding siblings collecting KDoc (`/** */`) or
/// consecutive line comments (`//`).
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut line_comments: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "block_comment" => {
                let text = node_text(source, p);
                if text.starts_with("/**") {
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

/// Returns the first `class_body` child of `node`, if any.
fn get_class_body(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .find(|child| child.kind() == "class_body");
    result
}

/// Returns true if this `class_declaration` is declared with the `interface` keyword.
fn is_interface(node: Node) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(|child| child.kind() == "interface");
    result
}

/// Returns true if this `class_declaration` has an `enum` class modifier.
fn is_enum_class(source: &[u8], node: Node) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let mut mod_cursor = child.walk();
            for modifier in child.children(&mut mod_cursor) {
                if modifier.kind() == "class_modifier" && node_text(source, modifier) == "enum" {
                    return true;
                }
            }
        }
    }
    false
}

/// Returns `(byte_end, line_end)` for a Kotlin class node.
///
/// Trims to the header line when the class body contains function declarations,
/// so `get_symbol` shows only the declaration rather than the full body. Enum
/// classes and interfaces are never trimmed.
fn class_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    // Only trim plain class_declaration — enums/interfaces stay intact.
    if node.kind() != "class_declaration" || is_interface(node) || is_enum_class(source, node) {
        return full;
    }

    let Some(body) = get_class_body(node) else {
        return full;
    };

    let has_methods = {
        let mut cursor = body.walk();
        let result = body
            .children(&mut cursor)
            .any(|child| child.kind() == "function_declaration");
        result
    };

    if !has_methods {
        return full;
    }

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
        language: Language::Kotlin,
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
        language: Language::Kotlin,
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
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = node_text(source, name_node).to_string();
            let kind = if is_interface(node) {
                SymbolKind::Interface
            } else if is_enum_class(source, node) {
                SymbolKind::Enum
            } else {
                SymbolKind::Class
            };
            push_type_symbol(source, node, path, &name, kind, symbols);
            if let Some(body) = get_class_body(node) {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_from_node(source, child, path, Some(&name), symbols);
                }
            }
        }
        "object_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = node_text(source, name_node).to_string();
            push_type_symbol(source, node, path, &name, SymbolKind::Class, symbols);
            if let Some(body) = get_class_body(node) {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_from_node(source, child, path, Some(&name), symbols);
                }
            }
        }
        "companion_object" => {
            // Companion object functions are attributed to the enclosing class.
            if let Some(body) = get_class_body(node) {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_from_node(source, child, path, class_name, symbols);
                }
            }
        }
        "function_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let fun_name = node_text(source, name_node).to_string();
            let (qualified, name, kind) = match class_name {
                Some(cls) => (format!("{cls}::{fun_name}"), fun_name, SymbolKind::Method),
                None => (fun_name.clone(), fun_name, SymbolKind::Function),
            };
            push_method_symbol(source, node, path, name, qualified, kind, symbols);
        }
        "secondary_constructor" => {
            if let Some(cls) = class_name {
                push_method_symbol(
                    source,
                    node,
                    path,
                    cls.to_string(),
                    format!("{cls}::{cls}"),
                    SymbolKind::Method,
                    symbols,
                );
            }
        }
        "type_alias" => {
            // tree-sitter-kotlin-ng does not expose a named "name" field on
            // type_alias; find the first named identifier child instead.
            let mut cursor = node.walk();
            let name_node = node
                .children(&mut cursor)
                .find(|child| child.kind() == "identifier");
            let Some(name_node) = name_node else {
                return;
            };
            let name = node_text(source, name_node).to_string();
            push_type_symbol(source, node, path, &name, SymbolKind::TypeAlias, symbols);
        }
        // Don't descend into function bodies.
        "function_body" | "block" => {}
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
            .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        KotlinParser.extract_symbols(source, &tree, Path::new("test.kt"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"class Foo";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_data_class() {
        let source = b"data class Point(val x: Int, val y: Int)";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Point");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_enum_class() {
        let source = b"enum class Color { RED, GREEN, BLUE }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_interface() {
        let source = b"interface Runnable { fun run() }";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "Runnable").unwrap();
        assert!(matches!(iface.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_object_declaration() {
        let source = b"object Singleton { fun get(): Int = 1 }";
        let symbols = parse_and_extract(source);
        let obj = symbols.iter().find(|s| s.name == "Singleton").unwrap();
        assert!(matches!(obj.kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = b"fun greet(name: String): String { return name }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_member_function() {
        let source = b"class Greeter { fun hello() {} }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "hello").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::hello");
    }

    #[test]
    fn test_extract_companion_object_function() {
        let source =
            b"class Foo {\n    companion object {\n        fun create(): Foo = Foo()\n    }\n}";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "create").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Foo::create");
    }

    #[test]
    fn test_extract_secondary_constructor() {
        let source = b"class Foo(val x: Int) { constructor(s: String) : this(s.length) }";
        let symbols = parse_and_extract(source);
        let ctor = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Method) && s.name == "Foo")
            .unwrap();
        assert_eq!(ctor.qualified, "Foo::Foo");
    }

    #[test]
    fn test_extract_type_alias() {
        let source = b"typealias StringList = List<String>";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "StringList");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_kdoc_extracted() {
        let source = b"/** Greets the world. */\nclass Hello";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Greets"), "doc={doc}");
    }

    #[test]
    fn test_line_comment_doc_extracted() {
        let source = b"// A simple counter.\nclass Counter";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("counter"), "doc={doc}");
    }

    #[test]
    fn test_class_trimmed_to_header_when_has_methods() {
        let source = b"class Service {\n    fun run() {\n        doWork()\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class with methods should be trimmed to header line"
        );
    }

    #[test]
    fn test_qualified_method_name() {
        let source = b"class Engine {\n    fun start() {}\n    fun stop() {}\n}";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "stop").unwrap();
        assert_eq!(start.qualified, "Engine::start");
        assert_eq!(stop.qualified, "Engine::stop");
    }

    #[test]
    fn test_multiple_top_level_classes() {
        let source = b"class Foo\nclass Bar";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_inner_class_extracted() {
        let source = b"class Outer { class Inner }";
        let symbols = parse_and_extract(source);
        assert!(symbols.iter().any(|s| s.name == "Outer"));
        assert!(symbols.iter().any(|s| s.name == "Inner"));
    }

    #[test]
    fn test_language_is_kotlin() {
        let source = b"class Foo";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols[0].language, Language::Kotlin);
    }
}
