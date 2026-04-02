use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct RubyParser;

impl LanguageParser for RubyParser {
    fn language(&self) -> Language {
        Language::Ruby
    }

    fn extensions(&self) -> &[&str] {
        &["rb"]
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

/// Walk backwards through preceding siblings collecting `#` line comments.
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

/// Returns `(byte_end, line_end)` for a class or module node.
///
/// Trims to the header line when the body contains method definitions,
/// leaving only the declaration line visible via `get_symbol`.
/// Classes/modules without methods are returned at full extent.
fn type_symbol_end(_source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_methods = {
        let mut cursor = body.walk();
        let result = body
            .children(&mut cursor)
            .any(|child| matches!(child.kind(), "method" | "singleton_method"));
        result
    };

    if !has_methods {
        return full;
    }

    // Trim to end of the header line (the line before the body starts).
    // body.start_position().row is the 0-indexed row of the first body statement,
    // which equals the 1-indexed line number of the header line.
    (body.start_byte(), body.start_position().row as u32)
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
        language: Language::Ruby,
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
    symbols: &mut Vec<Symbol>,
) {
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
        language: Language::Ruby,
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
        "class" => {
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
        "module" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Mod, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let method_name = node_text(source, name_node).to_string();
                let (qualified, name) = match class_name {
                    Some(cls) => (format!("{cls}#{method_name}"), method_name),
                    None => (method_name.clone(), method_name),
                };
                push_method_symbol(source, node, path, name, qualified, symbols);
            }
        }
        "singleton_method" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let method_name = node_text(source, name_node).to_string();
                let (qualified, name) = match class_name {
                    Some(cls) => (format!("{cls}.{method_name}"), method_name),
                    None => (method_name.clone(), method_name),
                };
                push_method_symbol(source, node, path, name, qualified, symbols);
            }
        }
        // Don't recurse into method bodies.
        "body_statement" => {}
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
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        RubyParser.extract_symbols(source, &tree, Path::new("test.rb"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"class Foo\nend";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_module() {
        let source = b"module Greetable\nend";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Greetable");
        assert!(matches!(symbols[0].kind, SymbolKind::Mod));
    }

    #[test]
    fn test_extract_method() {
        let source = b"class Greeter\n  def hello\n  end\nend";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "hello").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter#hello");
    }

    #[test]
    fn test_extract_singleton_method() {
        let source = b"class Greeter\n  def self.create\n  end\nend";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "create").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter.create");
    }

    #[test]
    fn test_extract_top_level_method() {
        let source = b"def greet(name)\nend";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Method));
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"# A greeting class.\nclass Hello\nend";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("greeting"), "doc={doc}");
    }

    #[test]
    fn test_class_body_trimmed_when_has_methods() {
        let source = b"class Service\n  def run\n    do_work\n  end\nend";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Service").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "class should be trimmed to header line"
        );
    }

    #[test]
    fn test_class_not_trimmed_when_no_methods() {
        let source = b"class Point\n  ORIGIN = [0, 0]\nend";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(
            cls.line_end > cls.line_start,
            "class without methods should not be trimmed"
        );
    }

    #[test]
    fn test_multiple_methods() {
        let source = b"class Engine\n  def start\n  end\n  def stop\n  end\nend";
        let symbols = parse_and_extract(source);
        let start = symbols.iter().find(|s| s.name == "start").unwrap();
        let stop = symbols.iter().find(|s| s.name == "stop").unwrap();
        assert_eq!(start.qualified, "Engine#start");
        assert_eq!(stop.qualified, "Engine#stop");
    }

    #[test]
    fn test_multiple_top_level_classes() {
        let source = b"class Foo\nend\nclass Bar\nend";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_line_numbers() {
        let source = b"class First\nend\n\nclass Second\nend";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"class Foo < Bar\n  def baz\n  end\nend";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "class Foo < Bar");
    }

    #[test]
    fn test_module_with_methods() {
        let source = b"module Helpers\n  def format(val)\n  end\nend";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "format").unwrap();
        assert_eq!(method.qualified, "Helpers#format");
    }

    #[test]
    fn test_initialize_extracted_as_method() {
        let source = b"class Point\n  def initialize(x, y)\n    @x = x\n    @y = y\n  end\nend";
        let symbols = parse_and_extract(source);
        let init = symbols.iter().find(|s| s.name == "initialize").unwrap();
        assert!(matches!(init.kind, SymbolKind::Method));
        assert_eq!(init.qualified, "Point#initialize");
    }

    #[test]
    fn test_method_with_predicate_suffix() {
        let source = b"class User\n  def valid?\n  end\nend";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "valid?").unwrap();
        assert_eq!(method.qualified, "User#valid?");
    }

    #[test]
    fn test_method_with_bang_suffix() {
        let source = b"class Record\n  def save!\n  end\nend";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "save!").unwrap();
        assert_eq!(method.qualified, "Record#save!");
    }

    #[test]
    fn test_nested_class_in_module() {
        let source = b"module Api\n  class Client\n    def get(path)\n    end\n  end\nend";
        let symbols = parse_and_extract(source);
        assert!(symbols
            .iter()
            .any(|s| s.name == "Api" && matches!(s.kind, SymbolKind::Mod)));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Client" && matches!(s.kind, SymbolKind::Class)));
        let method = symbols.iter().find(|s| s.name == "get").unwrap();
        assert_eq!(method.qualified, "Client#get");
    }

    #[test]
    fn test_scope_resolution_class_name() {
        let source = b"class Foo::Bar\n  def baz\n  end\nend";
        let symbols = parse_and_extract(source);
        let cls = symbols
            .iter()
            .find(|s| s.kind == SymbolKind::Class)
            .unwrap();
        assert_eq!(cls.name, "Foo::Bar");
    }
}
