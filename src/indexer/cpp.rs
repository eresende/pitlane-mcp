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
                push_symbol(
                    source,
                    node,
                    path,
                    name.clone(),
                    name.clone(),
                    kind,
                    symbols,
                );

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
}
