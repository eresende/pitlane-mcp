use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct CParser;

impl LanguageParser for CParser {
    fn language(&self) -> Language {
        Language::C
    }

    fn extensions(&self) -> &[&str] {
        &["c", "h"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        extract_from_node(source, tree.root_node(), path, &mut symbols);
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

/// Recursively unwrap declarator chains (pointer_declarator, function_declarator, etc.)
/// to find the innermost identifier. Returns the simple name.
fn declarator_name(source: &[u8], node: Node) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => {
            Some(node_text(source, node).to_string())
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
        language: Language::C,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc: None,
    });
}

fn extract_from_node(source: &[u8], node: Node, path: &std::path::Path, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_definition" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some(name) = declarator_name(source, decl) {
                    push_symbol(
                        source,
                        node,
                        path,
                        name.clone(),
                        name,
                        SymbolKind::Function,
                        symbols,
                    );
                }
            }
            // Don't recurse into function body
        }
        "struct_specifier" => {
            // Only extract named struct definitions (with a body)
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
                    SymbolKind::Struct,
                    symbols,
                );
            }
            // Don't recurse into struct body
        }
        "enum_specifier" => {
            // Only extract named enum definitions (with a body)
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
            // typedef — name comes from the declarator
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some(name) = declarator_name(source, decl) {
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
            // Don't recurse further — avoids double-extracting named structs inside typedefs
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
        // Skip these to avoid descending into function/struct internals
        "compound_statement" | "field_declaration_list" | "enumerator_list" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_from_node(source, child, path, symbols);
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
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        CParser.extract_symbols(source, &tree, Path::new("test.c"))
    }

    #[test]
    fn test_extract_function() {
        let symbols = parse_and_extract(b"int add(int a, int b) { return a + b; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "add");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_void_function() {
        let symbols = parse_and_extract(b"void greet(const char *name) {}");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_pointer_return_function() {
        let symbols = parse_and_extract(b"char *get_name(void) { return \"hello\"; }");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "get_name");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_struct() {
        let symbols = parse_and_extract(b"struct Point { int x; int y; };");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Point");
        assert!(matches!(symbols[0].kind, SymbolKind::Struct));
    }

    #[test]
    fn test_extract_enum() {
        let symbols = parse_and_extract(b"enum Color { RED, GREEN, BLUE };");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Color");
        assert!(matches!(symbols[0].kind, SymbolKind::Enum));
    }

    #[test]
    fn test_extract_typedef_simple() {
        let symbols = parse_and_extract(b"typedef int MyInt;");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MyInt");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_typedef_struct() {
        let symbols = parse_and_extract(b"typedef struct { int x; int y; } Point;");
        // anonymous struct inside typedef — only the typedef alias should be extracted
        let aliases: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::TypeAlias))
            .collect();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "Point");
    }

    #[test]
    fn test_extract_macro_define() {
        let symbols = parse_and_extract(b"#define MAX 100\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "MAX");
        assert!(matches!(symbols[0].kind, SymbolKind::Macro));
    }

    #[test]
    fn test_extract_macro_function() {
        let symbols = parse_and_extract(b"#define SQUARE(x) ((x) * (x))\n");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "SQUARE");
        assert!(matches!(symbols[0].kind, SymbolKind::Macro));
    }

    #[test]
    fn test_no_nested_function_symbols() {
        // Inner declarations inside a function body should not be extracted
        let source = b"int outer(void) { int inner_var = 0; return inner_var; }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "outer");
    }

    #[test]
    fn test_multiple_top_level() {
        let source =
            b"int add(int a, int b) { return a + b; }\nint sub(int a, int b) { return a - b; }";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"sub"));
    }

    #[test]
    fn test_line_numbers() {
        let source = b"int first(void) {}\nint second(void) {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
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
    fn test_struct_without_body_not_extracted() {
        // Forward declaration — no body, should not be extracted
        let symbols = parse_and_extract(b"struct Opaque;");
        assert!(
            symbols.iter().all(|s| s.name != "Opaque"),
            "forward declaration should not be extracted"
        );
    }

    #[test]
    fn test_mixed_c_file() {
        let source = b"\
#define VERSION 1\n\
typedef int Status;\n\
struct Node { int val; struct Node *next; };\n\
enum Dir { NORTH, SOUTH };\n\
int process(struct Node *n) { return n->val; }\n";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"VERSION"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Node"));
        assert!(names.contains(&"Dir"));
        assert!(names.contains(&"process"));
    }
}
