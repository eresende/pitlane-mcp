use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct BashParser;

impl LanguageParser for BashParser {
    fn language(&self) -> Language {
        Language::Bash
    }

    fn extensions(&self) -> &[&str] {
        &["sh", "bash"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let root = tree.root_node();
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            extract_top_level(source, child, path, &mut symbols);
        }
        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn get_signature(source: &[u8], node: Node) -> Option<String> {
    let text = node_text(source, node);
    let first_line = text.lines().next()?;
    Some(first_line.trim_end().to_string())
}

/// Walk backwards through preceding siblings collecting consecutive comment nodes.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut comments = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if p.kind() != "comment" {
            break;
        }
        comments.push(node_text(source, p).to_string());
        prev = p.prev_sibling();
    }
    if comments.is_empty() {
        return None;
    }
    comments.reverse();
    Some(comments.join("\n"))
}

fn extract_top_level(source: &[u8], node: Node, path: &std::path::Path, symbols: &mut Vec<Symbol>) {
    if node.kind() == "function_definition" {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = node_text(source, name_node).to_string();
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
            language: Language::Bash,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse_and_extract(source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        BashParser.extract_symbols(source, &tree, Path::new("test.sh"))
    }

    #[test]
    fn test_extract_function_keyword_syntax() {
        let source = b"function greet() {\n  echo 'hello'\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_posix_syntax() {
        let source = b"greet() {\n  echo 'hello'\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "greet");
        assert!(matches!(symbols[0].kind, SymbolKind::Function));
    }

    #[test]
    fn test_extract_multiple_functions() {
        let source = b"function foo() { echo foo; }\nfunction bar() { echo bar; }\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 2);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn test_doc_comment_extracted() {
        let source = b"# Greets the user.\nfunction greet() {\n  echo 'hello'\n}\n";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("Greets"), "doc={doc}");
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"function deploy() {\n  echo deploying\n}\n";
        let symbols = parse_and_extract(source);
        let sig = symbols[0].signature.as_deref().unwrap_or("");
        assert_eq!(sig, "function deploy() {");
    }

    #[test]
    fn test_line_numbers() {
        let source = b"function first() { echo 1; }\n\nfunction second() { echo 2; }\n";
        let symbols = parse_and_extract(source);
        let first = symbols.iter().find(|s| s.name == "first").unwrap();
        let second = symbols.iter().find(|s| s.name == "second").unwrap();
        assert!(first.line_start < second.line_start);
    }

    #[test]
    fn test_no_symbols_for_plain_script() {
        let source = b"#!/bin/bash\necho 'hello world'\n";
        let symbols = parse_and_extract(source);
        assert!(symbols.is_empty());
    }
}
