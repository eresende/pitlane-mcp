use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct ObjCParser;

impl LanguageParser for ObjCParser {
    fn language(&self) -> Language {
        Language::ObjC
    }

    fn extensions(&self) -> &[&str] {
        &["m", "mm"]
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

/// Returns the first `identifier` child that is a direct (non-nested) child of `node`.
fn first_ident_text<'a>(source: &'a [u8], node: Node) -> Option<&'a str> {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .find(|c| c.kind() == "identifier")
        .and_then(|c| c.utf8_text(source).ok());
    result
}

/// Returns true if `node` has a direct anonymous child with kind `kind`.
fn has_anon_child(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == kind);
    result
}

/// Builds the Objective-C selector string for a `method_declaration` or
/// `method_definition` node.
///
/// The selector is formed by concatenating `identifier` children (direct, not
/// nested inside `method_type` or `method_parameter`) and appending `:` after
/// each one that is immediately followed by a `method_parameter` child.
///
/// Examples:
/// - `- (void)simple`              → `"simple"`
/// - `- (void)setName:(NSString *)name`        → `"setName:"`
/// - `- (void)setName:(id)a age:(int)b` → `"setName:age:"`
fn build_selector(source: &[u8], node: Node) -> String {
    let child_count = node.child_count();
    let mut selector = String::new();
    for i in 0..child_count {
        let child = node.child(i as u32).unwrap();
        if child.kind() != "identifier" {
            continue;
        }
        let text = child.utf8_text(source).unwrap_or("");
        selector.push_str(text);
        // Append ':' if the next sibling is a method_parameter.
        let next_is_param = (i + 1 < child_count)
            && node
                .child((i + 1) as u32)
                .map(|n| n.kind() == "method_parameter")
                .unwrap_or(false);
        if next_is_param {
            selector.push(':');
        }
    }
    selector
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
    let end_pos = node.end_position();
    symbols.push(Symbol {
        id,
        name: name.to_string(),
        qualified: name.to_string(),
        kind,
        language: Language::ObjC,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
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
        language: Language::ObjC,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

/// Extracts the typedef name from a `type_definition` node.
///
/// Handles three common forms:
/// - `typedef NSString *MyAlias;`          → last `type_identifier` direct child
/// - `typedef void (^MyBlock)(args)`       → `type_identifier` inside
///   `function_declarator` → `parenthesized_declarator` → `block_pointer_declarator`
///
/// Returns `None` for typedef forms the grammar cannot parse cleanly (e.g.
/// NS_ENUM, NS_OPTIONS) — those produce ERROR nodes and are skipped.
fn typedef_name<'a>(source: &'a [u8], node: Node) -> Option<&'a str> {
    // Skip any typedef whose subtree contains an ERROR node.
    if subtree_has_error(node) {
        return None;
    }

    // Block pointer typedef: `typedef R (^Name)(params)` — name is nested inside
    // function_declarator → parenthesized_declarator → block_pointer_declarator.
    {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "function_declarator" {
                let mut c2 = child.walk();
                for c in child.children(&mut c2) {
                    if c.kind() == "parenthesized_declarator" {
                        let mut c3 = c.walk();
                        for cc in c.children(&mut c3) {
                            if cc.kind() == "block_pointer_declarator" {
                                let mut c4 = cc.walk();
                                let result = cc
                                    .children(&mut c4)
                                    .find(|n| n.kind() == "type_identifier")
                                    .and_then(|n| n.utf8_text(source).ok());
                                if result.is_some() {
                                    return result;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Simple or pointer typedef: last `type_identifier` that is a direct child,
    // or inside a `pointer_declarator` direct child.
    let mut cursor = node.walk();
    let mut last_name: Option<&'a str> = None;
    for child in node.children(&mut cursor) {
        match child.kind() {
            "type_identifier" => {
                last_name = child.utf8_text(source).ok();
            }
            "pointer_declarator" => {
                let mut c2 = child.walk();
                let result = child
                    .children(&mut c2)
                    .find(|n| n.kind() == "type_identifier")
                    .and_then(|n| n.utf8_text(source).ok());
                if result.is_some() {
                    last_name = result;
                }
            }
            _ => {}
        }
    }
    last_name
}

fn subtree_has_error(node: Node) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(subtree_has_error);
    result
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    class_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "class_interface" => {
            let is_category = has_anon_child(node, "(");
            if let Some(name) = first_ident_text(source, node) {
                let name = name.to_string();
                if !is_category {
                    // Primary @interface — emit a Class symbol.
                    push_type_symbol(source, node, path, &name, SymbolKind::Class, symbols);
                }
                // Recurse into the interface body for method declarations.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "method_declaration" {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "class_implementation" => {
            // @implementation blocks hold the actual method bodies.
            // Recurse into implementation_definition children.
            let ext_name = first_ident_text(source, node).map(|s| s.to_string());
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "implementation_definition" {
                    let mut c2 = child.walk();
                    for method_node in child.children(&mut c2) {
                        if method_node.kind() == "method_definition" {
                            extract_from_node(
                                source,
                                method_node,
                                path,
                                ext_name.as_deref(),
                                symbols,
                            );
                        }
                    }
                }
            }
        }
        "protocol_declaration" => {
            if let Some(name) = first_ident_text(source, node) {
                let name = name.to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::Interface, symbols);
                // Recurse for protocol method declarations.
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if child.kind() == "method_declaration" {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "method_declaration" | "method_definition" => {
            let selector = build_selector(source, node);
            if selector.is_empty() {
                return;
            }
            let (qualified, name) = match class_name {
                Some(cls) => (format!("{cls}::{selector}"), selector),
                None => (selector.clone(), selector),
            };
            push_method_symbol(source, node, path, name, qualified, symbols);
        }
        "function_definition" => {
            // C-style function — dig through declarator chain for the name.
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some(inner) = decl.child_by_field_name("declarator") {
                    if let Ok(name) = inner.utf8_text(source) {
                        let name = name.to_string();
                        let id = make_symbol_id(path, &name, &SymbolKind::Function);
                        let doc = get_doc_comment(source, node);
                        let signature = get_signature(source, node);
                        let start_pos = node.start_position();
                        let end_pos = node.end_position();
                        symbols.push(Symbol {
                            id,
                            name: name.clone(),
                            qualified: name,
                            kind: SymbolKind::Function,
                            language: Language::ObjC,
                            file: Arc::new(path.to_path_buf()),
                            byte_start: node.start_byte(),
                            byte_end: node.end_byte(),
                            line_start: start_pos.row as u32 + 1,
                            line_end: end_pos.row as u32 + 1,
                            signature,
                            doc,
                        });
                    }
                }
            }
        }
        "type_definition" => {
            if let Some(name) = typedef_name(source, node) {
                let name = name.to_string();
                push_type_symbol(source, node, path, &name, SymbolKind::TypeAlias, symbols);
            }
        }
        // Don't recurse into method/function bodies.
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
            .set_language(&tree_sitter_objc::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        ObjCParser.extract_symbols(source, &tree, Path::new("test.m"))
    }

    #[test]
    fn test_extract_class() {
        let source = b"@interface Foo : NSObject\n@end";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_protocol() {
        let source = b"@protocol Runnable\n- (void)run;\n@end";
        let symbols = parse_and_extract(source);
        let proto = symbols.iter().find(|s| s.name == "Runnable").unwrap();
        assert!(matches!(proto.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_protocol_method() {
        let source = b"@protocol Runnable\n- (void)run;\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "run").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Runnable::run");
    }

    #[test]
    fn test_extract_interface_method_declaration() {
        let source = b"@interface Greeter : NSObject\n- (NSString *)greet;\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::greet");
    }

    #[test]
    fn test_extract_implementation_method() {
        let source = b"@implementation Greeter\n- (NSString *)greet { return @\"Hi\"; }\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "greet").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::greet");
    }

    #[test]
    fn test_extract_class_method() {
        let source = b"@interface Greeter : NSObject\n+ (instancetype)create;\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "create").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Greeter::create");
    }

    #[test]
    fn test_extract_keyword_selector() {
        let source = b"@interface Foo : NSObject\n- (void)setName:(NSString *)name;\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "setName:").unwrap();
        assert_eq!(method.qualified, "Foo::setName:");
    }

    #[test]
    fn test_extract_multi_keyword_selector() {
        let source = b"@interface Foo : NSObject\n- (void)setName:(NSString *)n age:(int)a;\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "setName:age:").unwrap();
        assert_eq!(method.qualified, "Foo::setName:age:");
    }

    #[test]
    fn test_category_methods_attributed_to_class() {
        let source =
            b"@implementation Greeter (Formatting)\n- (NSString *)shout { return @\"HI\"; }\n@end";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "shout").unwrap();
        assert_eq!(method.qualified, "Greeter::shout");
    }

    #[test]
    fn test_category_interface_no_type_symbol() {
        // Category @interface should NOT emit a duplicate Class symbol.
        let source =
            b"@interface Greeter : NSObject\n@end\n@interface Greeter (Ext)\n- (void)extra;\n@end";
        let symbols = parse_and_extract(source);
        let classes: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .collect();
        assert_eq!(classes.len(), 1, "only one Class symbol for Greeter");
    }

    #[test]
    fn test_extract_typedef() {
        let source = b"typedef NSString *Name;";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Name");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_block_typedef() {
        let source = b"typedef void (^CompletionBlock)(BOOL success);";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "CompletionBlock");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_top_level_function() {
        let source = b"void greet(int x) {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"/// A greeting class.\n@interface Hello : NSObject\n@end";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Hello").unwrap();
        let doc = cls.doc.as_deref().unwrap_or("");
        assert!(doc.contains("greeting"), "doc={doc}");
    }

    #[test]
    fn test_multiple_classes() {
        let source = b"@interface Foo : NSObject\n@end\n@interface Bar : NSObject\n@end";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Class))
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_line_numbers() {
        let source = b"@interface First : NSObject\n@end\n\n@interface Second : NSObject\n@end";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "First").unwrap();
        let second = symbols.iter().find(|s| s.name == "Second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"@interface Foo : NSObject <Bar>\n- (void)baz;\n@end";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "@interface Foo : NSObject <Bar>");
    }

    #[test]
    fn test_ns_enum_skipped() {
        // NS_ENUM produces an ERROR node in the parse — should be skipped gracefully.
        let source = b"typedef NS_ENUM(NSInteger, Color) { ColorRed, ColorBlue };";
        let symbols = parse_and_extract(source);
        assert!(
            symbols.is_empty(),
            "NS_ENUM typedef should be skipped; got {symbols:?}"
        );
    }
}
