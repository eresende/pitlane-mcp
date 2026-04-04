use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct LuaParser;

impl LanguageParser for LuaParser {
    fn language(&self) -> Language {
        Language::Lua
    }

    fn extensions(&self) -> &[&str] {
        &["luau", "lua"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_from_node(source, child, path, &mut symbols);
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

/// Walk backwards through preceding siblings collecting contiguous Lua comments.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut docs = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() == "comment" {
            docs.push(node_text(source, p).to_string());
            prev = p.prev_sibling();
            continue;
        }
        break;
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
        language: Language::Lua,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

fn dotted_name(source: &[u8], node: Node) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(source, node).to_string()),
        "dot_index_expression" => {
            let table = dotted_name(source, node.child_by_field_name("table")?)?;
            let field = node_text(source, node.child_by_field_name("field")?).to_string();
            Some(format!("{table}.{field}"))
        }
        "field_type" => Some(node_text(source, node).to_string()),
        "generic_type" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    return dotted_name(source, child);
                }
            }
            None
        }
        _ => None,
    }
}

fn function_name_parts(source: &[u8], node: Node) -> Option<(String, String, SymbolKind)> {
    match node.kind() {
        "identifier" => {
            let name = node_text(source, node).to_string();
            Some((name.clone(), name, SymbolKind::Function))
        }
        "dot_index_expression" => {
            let qualified = dotted_name(source, node)?;
            let name = node_text(source, node.child_by_field_name("field")?).to_string();
            Some((name, qualified, SymbolKind::Method))
        }
        "method_index_expression" => {
            let table = dotted_name(source, node.child_by_field_name("table")?)?;
            let name = node_text(source, node.child_by_field_name("method")?).to_string();
            Some((name.clone(), format!("{table}:{name}"), SymbolKind::Method))
        }
        _ => None,
    }
}

fn type_alias_name(source: &[u8], node: Node) -> Option<String> {
    match node.kind() {
        "identifier" | "field_type" => Some(node_text(source, node).to_string()),
        "generic_type" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    return type_alias_name(source, child);
                }
            }
            None
        }
        _ => None,
    }
}

fn variable_name_parts(source: &[u8], node: Node) -> Option<(String, String, SymbolKind)> {
    match node.kind() {
        "identifier" => {
            let name = node_text(source, node).to_string();
            Some((name.clone(), name, SymbolKind::Function))
        }
        "dot_index_expression" => {
            let qualified = dotted_name(source, node)?;
            let name = node_text(source, node.child_by_field_name("field")?).to_string();
            Some((name, qualified, SymbolKind::Method))
        }
        _ => None,
    }
}

fn assignment_lists(node: Node) -> Option<(Node, Node)> {
    let mut variable_list = None;
    let mut expression_list = None;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "variable_list" => variable_list = Some(child),
            "expression_list" => expression_list = Some(child),
            _ => {}
        }
    }
    Some((variable_list?, expression_list?))
}

fn assignment_targets(source: &[u8], node: Node) -> Vec<(String, String, SymbolKind)> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(parts) = variable_name_parts(source, child) {
            names.push(parts);
        }
    }
    names
}

fn assignment_values(node: Node) -> Vec<Node> {
    let mut values = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            values.push(child);
        }
    }
    values
}

fn extract_function_assignments(
    source: &[u8],
    owner: Node,
    assignment: Node,
    path: &std::path::Path,
    symbols: &mut Vec<Symbol>,
) {
    let Some((variables, expressions)) = assignment_lists(assignment) else {
        return;
    };

    for ((name, qualified, kind), value) in assignment_targets(source, variables)
        .into_iter()
        .zip(assignment_values(expressions))
    {
        if value.kind() == "function_definition" {
            push_symbol(source, owner, path, name, qualified, kind, symbols);
        }
    }
}

fn extract_from_node(source: &[u8], node: Node, path: &std::path::Path, symbols: &mut Vec<Symbol>) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Some((name, qualified, kind)) = function_name_parts(source, name_node) {
                    push_symbol(source, node, path, name, qualified, kind, symbols);
                }
            }
        }
        "type_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Some(name) = type_alias_name(source, name_node) {
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
        "variable_declaration" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "assignment_statement" {
                    extract_function_assignments(source, node, child, path, symbols);
                }
            }
        }
        "assignment_statement" => extract_function_assignments(source, node, node, path, symbols),
        // Don't descend into function bodies.
        "block" => {}
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

    fn parse_and_extract(source: &[u8], path: &str) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_luau::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        LuaParser.extract_symbols(source, &tree, Path::new(path))
    }

    #[test]
    fn test_extract_local_function_declaration() {
        let source = b"local function greet(name: string): string\n    return name\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_table_function_declaration() {
        let source = b"function Greeter.new(name: string)\n    return { name = name }\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "new");
        assert!(matches!(symbols[0].kind, SymbolKind::Method));
        assert_eq!(symbols[0].qualified, "Greeter.new");
    }

    #[test]
    fn test_extract_colon_method_declaration() {
        let source = b"function Greeter:sayHello()\n    return self.name\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "sayHello");
        assert!(matches!(symbols[0].kind, SymbolKind::Method));
        assert_eq!(symbols[0].qualified, "Greeter:sayHello");
    }

    #[test]
    fn test_extract_local_function_assignment() {
        let source = b"local handler = function(player)\n    return player\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "handler");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert!(sig.starts_with("local handler = function("), "sig={sig}");
    }

    #[test]
    fn test_extract_table_function_assignment() {
        let source = b"Greeter.render = function(self)\n    return self.name\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "render");
        assert!(matches!(symbols[0].kind, SymbolKind::Method));
        assert_eq!(symbols[0].qualified, "Greeter.render");
    }

    #[test]
    fn test_extract_exported_type_alias() {
        let source = b"export type Point = { x: number, y: number }";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Point");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_extract_generic_type_alias_uses_base_name() {
        let source = b"type Result<T> = { ok: boolean, value: T }";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Result");
        assert!(matches!(symbols[0].kind, SymbolKind::TypeAlias));
    }

    #[test]
    fn test_doc_comment_attached() {
        let source = b"-- Greets the caller.\nlocal function greet()\nend";
        let symbols = parse_and_extract(source, "test.luau");
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Greets the caller"), "doc={doc}");
    }

    #[test]
    fn test_no_nested_functions() {
        let source = b"local function outer()\n    local function inner()\n    end\nend";
        let symbols = parse_and_extract(source, "test.luau");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "outer");
    }

    #[test]
    fn test_language_is_lua_for_lua_extension() {
        let source = b"local function greet()\nend";
        let symbols = parse_and_extract(source, "test.lua");
        assert_eq!(symbols[0].language, Language::Lua);
    }
}
