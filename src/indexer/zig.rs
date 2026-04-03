use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct ZigParser;

impl LanguageParser for ZigParser {
    fn language(&self) -> Language {
        Language::Zig
    }

    fn extensions(&self) -> &[&str] {
        &["zig"]
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

/// Walk backwards through preceding siblings collecting doc comments (`///`).
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut docs: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            let text = node_text(source, p);
            if text.starts_with("///") || text.starts_with("//!") {
                docs.push(text.to_string());
                prev = p.prev_sibling();
                continue;
            }
            break;
        } else {
            break;
        }
    }
    if docs.is_empty() {
        return None;
    }
    docs.reverse();
    Some(docs.join("\n"))
}

fn push_symbol(
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
        language: Language::Zig,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

/// Returns the first identifier child of a variable_declaration (the variable name).
fn var_decl_name<'a>(source: &'a [u8], node: Node) -> Option<&'a str> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            return Some(node_text(source, child));
        }
    }
    None
}

/// Returns the container node (struct/enum/union) that is the RHS of a variable_declaration,
/// if there is one. Because `expression` is a tree-sitter supertype, `struct_declaration` etc.
/// appear as direct children of `variable_declaration`.
fn var_decl_container(node: Node) -> Option<Node> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "struct_declaration"
            | "enum_declaration"
            | "union_declaration"
            | "error_set_declaration"
            | "opaque_declaration" => return Some(child),
            _ => {}
        }
    }
    None
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    container_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_declaration" | "function_signature" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(source, name_node).to_string();
                let (kind, qualified) = match container_name {
                    Some(cn) => (SymbolKind::Method, format!("{cn}.{fn_name}")),
                    None => (SymbolKind::Function, fn_name.clone()),
                };
                push_symbol(source, node, path, fn_name, qualified, kind, symbols);
            }
        }
        "variable_declaration" => {
            let Some(name) = var_decl_name(source, node) else {
                return;
            };
            let name = name.to_string();

            if let Some(container) = var_decl_container(node) {
                // Named type declaration: const Foo = struct/enum/union { ... };
                let kind = match container.kind() {
                    "enum_declaration" | "error_set_declaration" => SymbolKind::Enum,
                    _ => SymbolKind::Struct,
                };
                push_symbol(
                    source,
                    node,
                    path,
                    name.clone(),
                    name.clone(),
                    kind,
                    symbols,
                );
                // Recurse into container body for methods (only at top level).
                if container_name.is_none() {
                    let mut cursor = container.walk();
                    for child in container.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            } else if container_name.is_none() {
                // Top-level const/var that is not a named container type.
                push_symbol(
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::Const,
                    symbols,
                );
            }
        }
        "test_declaration" => {
            // Name is an optional string or identifier child.
            let test_name = {
                let mut cursor = node.walk();
                let mut name = None;
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "string" => {
                            let raw = node_text(source, child);
                            // Strip surrounding quotes from the string literal.
                            name = Some(raw.trim_matches('"').to_string());
                            break;
                        }
                        "identifier" => {
                            name = Some(node_text(source, child).to_string());
                            break;
                        }
                        _ => {}
                    }
                }
                name.unwrap_or_else(|| "_".to_string())
            };
            push_symbol(
                source,
                node,
                path,
                test_name.clone(),
                test_name,
                SymbolKind::Function,
                symbols,
            );
        }
        // Skip comptime blocks and blocks (don't recurse into function bodies).
        "comptime_declaration" | "block" => {}
        _ => {
            if container_name.is_none() {
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
            .set_language(&tree_sitter_zig::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        ZigParser.extract_symbols(source, &tree, Path::new("test.zig"))
    }

    #[test]
    fn test_extract_function() {
        let source = b"pub fn hello() void {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_private_function() {
        let source = b"fn greet(name: []const u8) void {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_struct() {
        let source = b"const Point = struct { x: f64, y: f64 };";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(matches!(s.kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_enum() {
        let source = b"const Color = enum { Red, Green, Blue };";
        let symbols = parse_and_extract(source);
        let e = symbols.iter().find(|s| s.name == "Color").unwrap();
        assert!(matches!(e.kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_union() {
        let source = b"const Value = union { int: i32, float: f32 };";
        let symbols = parse_and_extract(source);
        let u = symbols.iter().find(|s| s.name == "Value").unwrap();
        assert!(matches!(u.kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_method() {
        let source =
            b"const Point = struct { x: f64, pub fn init(x: f64) Point { return .{ .x = x }; } };";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "init").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Point.init");
    }

    #[test]
    fn test_extract_top_level_const() {
        let source = b"const MAX: u32 = 100;";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MAX");
        assert!(matches!(symbols[0].kind, SymbolKind::Const));
    }

    #[test]
    fn test_extract_test_declaration() {
        let source = b"test \"adds two numbers\" { }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "adds two numbers");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_anonymous_test() {
        let source = b"test { }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "_");
    }

    #[test]
    fn test_multiple_top_level_items() {
        let source = b"const Foo = struct {};\nconst Bar = enum { A, B };\nfn run() void {}";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
        assert!(names.contains(&"run"));
    }

    #[test]
    fn test_doc_comment_attached() {
        let source = b"/// Creates a new point.\npub fn init() void {}";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Creates a new point"), "doc={doc}");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"pub fn complex(\n    a: i32,\n    b: i32,\n) void {}";
        let symbols = parse_and_extract(source);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "pub fn complex(");
    }

    #[test]
    fn test_method_not_extracted_as_top_level_function() {
        let source = b"const S = struct { fn foo(self: S) void {} };";
        let symbols = parse_and_extract(source);
        let fns: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function))
            .collect();
        assert!(fns.is_empty(), "foo should be a method, not a function");
        let methods: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Method))
            .collect();
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].qualified, "S.foo");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"fn first() void {}\n\nfn second() void {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_function_signature_extern() {
        let source = b"extern fn printf(fmt: [*:0]const u8, ...) c_int;";
        let symbols = parse_and_extract(source);
        let f = symbols.iter().find(|s| s.name == "printf").unwrap();
        assert!(matches!(f.kind, SymbolKind::Function));
    }
}
