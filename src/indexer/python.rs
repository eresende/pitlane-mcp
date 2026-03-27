use tree_sitter::{Node, Tree};

use crate::indexer::language::{Language, LanguageParser, Symbol, SymbolKind, make_symbol_id};

pub struct PythonParser;

impl LanguageParser for PythonParser {
    fn language(&self) -> Language {
        Language::Python
    }

    fn extensions(&self) -> &[&str] {
        &["py"]
    }

    fn extract_symbols(
        &self,
        source: &[u8],
        tree: &Tree,
        path: &std::path::Path,
    ) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        extract_from_node(source, root, path, None, &mut symbols);
        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn get_name(source: &[u8], node: Node) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(source, n).to_string())
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    let first_line = text.lines().next()?;
    Some(first_line.trim_end().to_string())
}

fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    // In Python, doc strings are expression statements with string literals
    // Look at the first statement of the body
    let body = node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for child in body.children(&mut cursor) {
        if child.kind() == "expression_statement" {
            let mut inner_cursor = child.walk();
            for inner in child.children(&mut inner_cursor) {
                if inner.kind() == "string" {
                    return Some(node_text(source, inner).to_string());
                }
            }
        }
        break; // Only check first statement
    }
    None
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
            if let Some(name) = get_name(source, node) {
                let (kind, qualified) = if let Some(cls) = class_name {
                    (SymbolKind::Method, format!("{}::{}", cls, name))
                } else {
                    (SymbolKind::Function, name.clone())
                };

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
                    language: Language::Python,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into function body (nested functions not extracted)
        }
        "class_definition" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Class);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name.clone(),
                    kind: SymbolKind::Class,
                    language: Language::Python,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });

                // Recurse into class body to find methods
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        if child.kind() == "function_definition" {
                            extract_from_node(source, child, path, Some(&name), symbols);
                        } else if child.kind() == "decorated_definition" {
                            // Handle decorated methods
                            let mut inner_cursor = child.walk();
                            for inner in child.children(&mut inner_cursor) {
                                if inner.kind() == "function_definition" {
                                    extract_from_node(source, inner, path, Some(&name), symbols);
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // Recurse into top-level nodes
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() != "block" {
                    extract_from_node(source, child, path, class_name, symbols);
                }
            }
        }
    }
}
