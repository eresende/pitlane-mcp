use tree_sitter::{Node, Tree};

use crate::indexer::language::{Language, LanguageParser, Symbol, SymbolKind, make_symbol_id};

pub struct RustParser;

impl LanguageParser for RustParser {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn extensions(&self) -> &[&str] {
        &["rs"]
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

fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    // Look at preceding siblings for doc comments
    let mut docs = Vec::new();
    let parent = node.parent()?;
    let mut cursor = parent.walk();

    let mut prev_was_doc = false;
    let mut prev_nodes: Vec<Node> = Vec::new();

    for child in parent.children(&mut cursor) {
        if child.id() == node.id() {
            break;
        }
        prev_nodes.push(child);
    }

    // Walk backwards through preceding siblings collecting doc comments
    let mut collecting = true;
    for sibling in prev_nodes.iter().rev() {
        if !collecting {
            break;
        }
        let kind = sibling.kind();
        if kind == "line_comment" || kind == "block_comment" {
            let text = node_text(source, *sibling);
            if text.starts_with("///") || text.starts_with("/**") {
                docs.push(text.to_string());
                prev_was_doc = true;
            } else {
                collecting = false;
            }
        } else if kind != "\n" && !sibling.is_extra() {
            // Non-comment node: stop collecting if we already have docs
            if prev_was_doc {
                break;
            }
            collecting = false;
        }
    }

    if docs.is_empty() {
        return None;
    }

    docs.reverse();
    Some(docs.join("\n"))
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

fn extract_impl_type_name(source: &[u8], node: Node) -> String {
    // Look for trait_type and type fields on impl_item
    let trait_type = node.child_by_field_name("trait");
    let self_type = node.child_by_field_name("type");

    match (trait_type, self_type) {
        (Some(tr), Some(ty)) => {
            format!("impl {} for {}", node_text(source, tr), node_text(source, ty))
        }
        (None, Some(ty)) => {
            format!("impl {}", node_text(source, ty))
        }
        _ => "impl".to_string(),
    }
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    impl_type: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "function_item" => {
            if let Some(name) = get_name(source, node) {
                let (kind, qualified) = if let Some(impl_name) = impl_type {
                    (
                        SymbolKind::Method,
                        format!("{}::{}", impl_name.trim_start_matches("impl ").split(" for ").last().unwrap_or(impl_name), name),
                    )
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
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into function bodies
        }
        "struct_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Struct);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Struct,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "enum_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Enum);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Enum,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "trait_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Trait);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Trait,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
            // Don't recurse into trait body (would need to handle trait methods differently)
        }
        "impl_item" => {
            let impl_name = extract_impl_type_name(source, node);
            let qualified = impl_name.clone();
            let id = make_symbol_id(path, &qualified, &SymbolKind::Impl);
            let doc = get_doc_comment(source, node);
            let signature = get_signature(source, node);
            let start_pos = node.start_position();
            let end_pos = node.end_position();

            // Extract a short name for the impl (just the type name)
            let name = node.child_by_field_name("type")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_else(|| "impl".to_string());

            symbols.push(Symbol {
                id,
                name,
                qualified,
                kind: SymbolKind::Impl,
                language: Language::Rust,
                file: path.to_path_buf(),
                byte_start: node.start_byte(),
                byte_end: node.end_byte(),
                line_start: start_pos.row as u32 + 1,
                line_end: end_pos.row as u32 + 1,
                signature,
                doc,
            });

            // Extract the type name for method qualification
            let type_name = node.child_by_field_name("type")
                .map(|n| node_text(source, n).to_string())
                .unwrap_or_else(|| "Unknown".to_string());

            // Recurse into impl body to find methods
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "declaration_list" {
                    let mut inner_cursor = child.walk();
                    for inner_child in child.children(&mut inner_cursor) {
                        if inner_child.kind() == "function_item" {
                            extract_from_node(source, inner_child, path, Some(&type_name), symbols);
                        }
                    }
                }
            }
        }
        "mod_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Mod);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Mod,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
                // Don't recurse into mod body
            }
        }
        "macro_rules" => {
            if let Some(name) = node.child_by_field_name("name")
                .map(|n| node_text(source, n).to_string())
                .or_else(|| {
                    // Sometimes macro name is in a different position
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        if child.kind() == "identifier" {
                            return Some(node_text(source, child).to_string());
                        }
                    }
                    None
                })
            {
                let id = make_symbol_id(path, &name, &SymbolKind::Macro);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Macro,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "const_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::Const);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::Const,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        "type_item" => {
            if let Some(name) = get_name(source, node) {
                let id = make_symbol_id(path, &name, &SymbolKind::TypeAlias);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                symbols.push(Symbol {
                    id,
                    name: name.clone(),
                    qualified: name,
                    kind: SymbolKind::TypeAlias,
                    language: Language::Rust,
                    file: path.to_path_buf(),
                    byte_start: node.start_byte(),
                    byte_end: node.end_byte(),
                    line_start: start_pos.row as u32 + 1,
                    line_end: end_pos.row as u32 + 1,
                    signature,
                    doc,
                });
            }
        }
        _ => {
            // Recurse into other nodes at top level
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                // Don't recurse into function bodies / blocks
                if child.kind() != "block" {
                    extract_from_node(source, child, path, impl_type, symbols);
                }
            }
        }
    }
}
