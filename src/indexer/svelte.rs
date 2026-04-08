use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::javascript::JavaScriptParser;
use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol};
use crate::indexer::typescript::TypeScriptParser;

pub struct SvelteParser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptBlockLanguage {
    JavaScript,
    TypeScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScriptBlock {
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: u32,
    pub language: ScriptBlockLanguage,
}

impl LanguageParser for SvelteParser {
    fn language(&self) -> Language {
        Language::Svelte
    }

    fn extensions(&self) -> &[&str] {
        &["svelte"]
    }

    fn extract_symbols(&self, source: &[u8], tree: &Tree, path: &std::path::Path) -> Vec<Symbol> {
        let mut symbols = Vec::new();

        for block in collect_script_blocks(source, tree.root_node()) {
            let script_source = &source[block.byte_start..block.byte_end];
            let mut parser = tree_sitter::Parser::new();
            let language = match block.language {
                ScriptBlockLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
                ScriptBlockLanguage::TypeScript => {
                    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
                }
            };

            if parser.set_language(&language).is_err() {
                continue;
            }

            let Some(tree) = parser.parse(script_source, None) else {
                continue;
            };

            let mut extracted = match block.language {
                ScriptBlockLanguage::JavaScript => {
                    JavaScriptParser.extract_symbols(script_source, &tree, path)
                }
                ScriptBlockLanguage::TypeScript => {
                    TypeScriptParser.extract_symbols(script_source, &tree, path)
                }
            };

            for symbol in &mut extracted {
                symbol.id = make_symbol_id(path, &symbol.qualified, &symbol.kind);
                symbol.language = Language::Svelte;
                symbol.file = Arc::new(path.to_path_buf());
                symbol.byte_start += block.byte_start;
                symbol.byte_end += block.byte_start;
                symbol.line_start += block.line_start - 1;
                symbol.line_end += block.line_start - 1;
            }

            symbols.extend(extracted);
        }

        symbols
    }
}

fn node_text<'a>(source: &'a [u8], node: Node) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn parse_lang_attr(value: &str) -> ScriptBlockLanguage {
    match value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_ascii_lowercase()
        .as_str()
    {
        "ts" | "typescript" | "text/typescript" => ScriptBlockLanguage::TypeScript,
        _ => ScriptBlockLanguage::JavaScript,
    }
}

fn script_block_language(source: &[u8], script_element: Node) -> ScriptBlockLanguage {
    let mut cursor = script_element.walk();
    for child in script_element.children(&mut cursor) {
        if child.kind() != "start_tag" {
            continue;
        }

        let mut tag_cursor = child.walk();
        for attr in child.children(&mut tag_cursor) {
            if attr.kind() != "attribute" {
                continue;
            }

            let text = node_text(source, attr);
            if let Some((name, value)) = text.split_once('=') {
                if name.trim().eq_ignore_ascii_case("lang") {
                    return parse_lang_attr(value);
                }
            }
        }
    }

    ScriptBlockLanguage::JavaScript
}

pub(crate) fn collect_script_blocks(source: &[u8], root: Node) -> Vec<ScriptBlock> {
    let mut blocks = Vec::new();
    collect_script_blocks_from_node(source, root, &mut blocks);
    blocks
}

fn collect_script_blocks_from_node(source: &[u8], node: Node, blocks: &mut Vec<ScriptBlock>) {
    if node.kind() == "script_element" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "raw_text" {
                blocks.push(ScriptBlock {
                    byte_start: child.start_byte(),
                    byte_end: child.end_byte(),
                    line_start: child.start_position().row as u32 + 1,
                    language: script_block_language(source, node),
                });
                break;
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_script_blocks_from_node(source, child, blocks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::language::SymbolKind;
    use std::path::Path;

    fn parse_and_extract(path: &str, source: &[u8]) -> Vec<Symbol> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_svelte_ng::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        SvelteParser.extract_symbols(source, &tree, Path::new(path))
    }

    #[test]
    fn test_extracts_javascript_symbols_from_script_block() {
        let source = br#"<script>
function greet() {}
class Greeter {
  wave() {}
}
</script>

<div>{greet()}</div>
"#;

        let symbols = parse_and_extract("Component.svelte", source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();

        assert!(names.contains(&"greet"));
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"wave"));
        assert!(symbols.iter().all(|s| s.language == Language::Svelte));
    }

    #[test]
    fn test_extracts_typescript_symbols_from_lang_ts_script_block() {
        let source = br#"<script lang='ts'>
export interface Props { id: string; }
export type Status = 'draft' | 'published';
export enum Mode { View }
</script>
"#;

        let symbols = parse_and_extract("Component.svelte", source);
        assert!(symbols
            .iter()
            .any(|s| s.name == "Props" && s.kind == SymbolKind::Interface));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Status" && s.kind == SymbolKind::TypeAlias));
        assert!(symbols
            .iter()
            .any(|s| s.name == "Mode" && s.kind == SymbolKind::Enum));
    }

    #[test]
    fn test_maps_symbol_offsets_back_to_original_svelte_file() {
        let source = br#"<script>
function greet() {}
</script>
<div />
"#;

        let symbols = parse_and_extract("Component.svelte", source);
        let greet = symbols.iter().find(|s| s.name == "greet").unwrap();

        assert_eq!(greet.line_start, 2);
        assert_eq!(greet.line_end, 2);
        assert_eq!(
            &source[greet.byte_start..greet.byte_end],
            b"function greet() {}"
        );
    }

    #[test]
    fn test_extracts_symbols_from_multiple_script_blocks() {
        let source = br#"<script context='module' lang='ts'>
export interface LoaderData { slug: string; }
</script>

<script>
function hydrate() {}
</script>
"#;

        let symbols = parse_and_extract("Component.svelte", source);
        assert!(symbols.iter().any(|s| s.name == "LoaderData"));
        assert!(symbols.iter().any(|s| s.name == "hydrate"));
    }

    #[test]
    fn test_language_is_svelte() {
        let source = br#"<script>function f() {}</script>"#;
        let symbols = parse_and_extract("Component.svelte", source);
        assert_eq!(symbols[0].language, Language::Svelte);
    }
}
