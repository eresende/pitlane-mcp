use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct CppParser;

impl LanguageParser for CppParser {
    fn language(&self) -> Language {
        Language::Cpp
    }

    fn extensions(&self) -> &[&str] {
        &["cpp", "cc", "cxx", "hpp", "hxx"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        extract_from_node(source, tree.root_node(), path, None, &mut symbols);
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

/// Returns the immediately preceding doc comment (`/** ... */` or `///`), if any.
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
            if text.starts_with("/**") || text.starts_with("///") {
                return Some(text.to_string());
            }
            break;
        } else if !sibling.is_extra() {
            break;
        }
    }

    None
}

/// Returns `(byte_end, line_end)` for a C++ class or struct node.
///
/// For classes/structs with inline method definitions, trims to just the header
/// line so that `get_symbol` returns only the declaration, not the full body.
/// Classes/structs with no inline methods (field-only or empty) are returned
/// at full extent.
fn class_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_methods = {
        let mut cursor = body.walk();
        let x = body
            .children(&mut cursor)
            .any(|child| child.kind() == "function_definition");
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

/// Recursively unwrap declarator chains to find the innermost name.
/// Returns `(simple_name, qualified_name)`.
///
/// For plain identifiers: `("foo", "foo")`
/// For qualified identifiers (`MyClass::method`): `("method", "MyClass::method")`
fn declarator_name(source: &[u8], node: Node) -> Option<(String, String)> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            let name = node_text(source, node).to_string();
            Some((name.clone(), name))
        }
        "qualified_identifier" => {
            let full = node_text(source, node).to_string();
            // Strip any trailing template arguments (e.g. "Foo<T>::bar" → "bar")
            let simple = node
                .child_by_field_name("name")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_else(|| full.split("::").last().unwrap_or(&full).to_string());
            Some((simple, full))
        }
        "destructor_name" => {
            // ~ClassName — emit as-is
            let name = node_text(source, node).to_string();
            Some((name.clone(), name))
        }
        _ => node
            .child_by_field_name("declarator")
            .and_then(|d| declarator_name(source, d)),
    }
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
    let signature = get_signature(source, node);
    let start_pos = node.start_position();
    let end_pos = node.end_position();
    symbols.push(Symbol {
        id,
        name,
        qualified,
        kind,
        language: Language::Cpp,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc: None,
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
        "function_definition" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some((simple, qualified)) = declarator_name(source, decl) {
                    let (kind, name, qualified) = if let Some(cls) = class_name {
                        // Inside a class body — always a method
                        let q = format!("{}::{}", cls, simple);
                        (SymbolKind::Method, simple, q)
                    } else if qualified.contains("::") {
                        // Top-level out-of-class definition like `Foo::bar()`
                        (SymbolKind::Method, simple, qualified)
                    } else {
                        (SymbolKind::Function, simple.clone(), simple)
                    };
                    push_symbol(source, node, path, name, qualified, kind, symbols);
                }
            }
            // Don't recurse into function body
        }
        "class_specifier" | "struct_specifier" => {
            // Extract the class/struct itself, then recurse into its body for methods
            let kind = if node.kind() == "class_specifier" {
                SymbolKind::Class
            } else {
                SymbolKind::Struct
            };

            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                let id = make_symbol_id(path, &name, &kind);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let (byte_end, line_end) = class_symbol_end(source, node);
                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name.clone(),
                    kind,
                    language: Language::Cpp,
                    file: Arc::new(path.to_path_buf()),
                    byte_start: node.start_byte(),
                    byte_end,
                    line_start: start_pos.row as u32 + 1,
                    line_end,
                    signature,
                    doc,
                });

                // Recurse into the body to pick up methods
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            } else if node.child_by_field_name("body").is_some() {
                // Anonymous struct/class — recurse but don't emit a symbol
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, class_name, symbols);
                    }
                }
            }
        }
        "enum_specifier" => {
            if let (Some(name_node), Some(_body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            ) {
                let name = node_text(source, name_node).to_string();
                push_symbol(
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::Enum,
                    symbols,
                );
            }
        }
        "type_definition" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some((name, _)) = declarator_name(source, decl) {
                    push_symbol(
                        source,
                        node,
                        path,
                        name.clone(),
                        name,
                        SymbolKind::TypeAlias,
                        symbols,
                    );
                }
            }
        }
        "preproc_def" | "preproc_function_def" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_symbol(
                    source,
                    node,
                    path,
                    name.clone(),
                    name,
                    SymbolKind::Macro,
                    symbols,
                );
            }
        }
        "namespace_definition" => {
            // Don't emit the namespace as a symbol — just recurse into its body
            if let Some(body) = node.child_by_field_name("body") {
                let mut cursor = body.walk();
                for child in body.children(&mut cursor) {
                    extract_from_node(source, child, path, class_name, symbols);
                }
            }
        }
        // Don't descend into these
        "compound_statement" | "field_declaration_list" | "enumerator_list" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_from_node(source, child, path, class_name, symbols);
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
            .set_language(&tree_sitter_cpp::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        CppParser.extract_symbols(source, &tree, Path::new("test.cpp"))
    }

    #[test]
    fn test_extract_function() {
        let symbols = parse_and_extract(b"int add(int a, int b) { return a + b; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "add");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_class_and_methods() {
        let source = b"\
class Greeter {\n\
public:\n\
    void hello() {}\n\
    void bye() {}\n\
};\n";
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
    fn test_extract_struct_with_methods() {
        let source = b"\
struct Point {\n\
    int x, y;\n\
    int sum() { return x + y; }\n\
};\n";
        let symbols = parse_and_extract(source);

        let struct_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Struct))
            .unwrap();
        assert_eq!(struct_sym.name, "Point");

        let method = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Method))
            .unwrap();
        assert_eq!(method.qualified, "Point::sum");
    }

    #[test]
    fn test_extract_out_of_class_method() {
        let source = b"void Greeter::hello() {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "hello");
        assert_eq!(symbols[0].qualified, "Greeter::hello");
        assert!(matches!(symbols[0].kind, SymbolKind::Method));
    }

    #[test]
    fn test_extract_enum() {
        let symbols = parse_and_extract(b"enum Color { RED, GREEN, BLUE };");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_typedef() {
        let symbols = parse_and_extract(b"typedef int MyInt;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyInt");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_macro() {
        let symbols = parse_and_extract(b"#define MAX 100\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MAX");
        assert!(matches!(symbols[0].kind, SymbolKind::Macro));
    }

    #[test]
    fn test_namespace_contents_extracted() {
        let source = b"\
namespace math {\n\
    int add(int a, int b) { return a + b; }\n\
}\n";
        let symbols = parse_and_extract(source);
        // Namespace itself is not a symbol, but its contents are
        assert!(
            symbols.iter().all(|s| s.name != "math"),
            "namespace should not produce a symbol"
        );
        let func = symbols.iter().find(|s| s.name == "add");
        assert!(
            func.is_some(),
            "function inside namespace should be extracted"
        );
    }

    #[test]
    fn test_no_nested_function_body_symbols() {
        let source = b"int outer() { int x = 0; return x; }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "outer");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"int first() {}\nint second() {}";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert_eq!(first.line_start, 1);
        assert_eq!(second.line_start, 2);
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"int add(\n    int a,\n    int b\n) {\n    return a + b;\n}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "int add(");
    }

    #[test]
    fn test_mixed_cpp_file() {
        let source = b"\
#define VERSION 2\n\
typedef int Status;\n\
enum Dir { NORTH, SOUTH };\n\
class Router {\n\
public:\n\
    void route() {}\n\
};\n\
int main() { return 0; }\n";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Dir"));
        assert!(names.contains(&"Router"));
        assert!(names.contains(&"route"));
        assert!(names.contains(&"main"));
    }

    #[test]
    fn test_pointer_return_function() {
        let symbols = parse_and_extract(b"char *get_name() { return \"hello\"; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "get_name");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    /// A class with inline methods should be trimmed to just the header line.
    #[test]
    fn test_class_with_methods_trimmed_to_header() {
        let source = b"class Greeter {\npublic:\n    void hello() {}\n    void bye() {}\n};\n";
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
        assert!(
            source_str.starts_with("class Greeter {"),
            "got: {source_str:?}"
        );
        assert!(!source_str.contains("hello"), "body should not be included");
    }

    /// A struct with inline methods should be trimmed to just the header line.
    #[test]
    fn test_struct_with_methods_trimmed_to_header() {
        let source = b"struct Point {\n    int x, y;\n    int sum() { return x + y; }\n};\n";
        let symbols = parse_and_extract(source);
        let struct_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Struct))
            .unwrap();
        assert_eq!(struct_sym.line_start, 1);
        assert_eq!(
            struct_sym.line_end, 1,
            "struct symbol should be trimmed to header only"
        );
        let source_str =
            std::str::from_utf8(&source[struct_sym.byte_start..struct_sym.byte_end]).unwrap();
        assert!(
            source_str.starts_with("struct Point {"),
            "got: {source_str:?}"
        );
        assert!(!source_str.contains("sum"), "body should not be included");
    }

    /// A multi-line class with only field declarations should NOT be trimmed.
    #[test]
    fn test_field_only_class_not_trimmed() {
        let source = b"class Config {\npublic:\n    int timeout;\n    int retries;\n};\n";
        let symbols = parse_and_extract(source);
        let class_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        assert!(
            class_sym.line_end > class_sym.line_start,
            "field-only class should not be trimmed (line_start={} line_end={})",
            class_sym.line_start,
            class_sym.line_end,
        );
        let source_str =
            std::str::from_utf8(&source[class_sym.byte_start..class_sym.byte_end]).unwrap();
        assert!(source_str.contains("retries"), "fields should be included");
    }

    /// A multi-line struct with only field declarations should NOT be trimmed.
    #[test]
    fn test_field_only_struct_not_trimmed() {
        let source = b"struct Rect {\n    int x;\n    int y;\n    int w;\n    int h;\n};\n";
        let symbols = parse_and_extract(source);
        let struct_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Struct))
            .unwrap();
        assert!(
            struct_sym.line_end > struct_sym.line_start,
            "field-only struct should not be trimmed (line_start={} line_end={})",
            struct_sym.line_start,
            struct_sym.line_end,
        );
        let source_str =
            std::str::from_utf8(&source[struct_sym.byte_start..struct_sym.byte_end]).unwrap();
        assert!(source_str.contains("int h"), "fields should be included");
    }

    /// A Doxygen block comment preceding a class should be captured as `doc`.
    #[test]
    fn test_doxygen_block_comment_on_class() {
        let source =
            b"/** Manages connections. */\nclass ConnManager {\n    void connect() {}\n};\n";
        let symbols = parse_and_extract(source);
        let class_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Class))
            .unwrap();
        let doc = class_sym.doc.as_deref().unwrap_or("");
        assert!(
            doc.contains("Manages connections"),
            "doc should contain class comment, got: {doc:?}"
        );
    }

    /// A `///` line comment preceding a struct should be captured as `doc`.
    #[test]
    fn test_doxygen_line_comment_on_struct() {
        let source = b"/// A 2-D point.\nstruct Vec2 {\n    float x;\n    float y;\n};\n";
        let symbols = parse_and_extract(source);
        let struct_sym = symbols
            .iter()
            .find(|s| matches!(s.kind, SymbolKind::Struct))
            .unwrap();
        let doc = struct_sym.doc.as_deref().unwrap_or("");
        assert!(
            doc.contains("2-D point"),
            "doc should contain struct comment, got: {doc:?}"
        );
    }
}
