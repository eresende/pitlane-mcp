use std::sync::Arc;

use tree_sitter::{Node, Tree};

use crate::indexer::language::{make_symbol_id, Language, LanguageParser, Symbol, SymbolKind};

pub struct SolidityParser;

impl LanguageParser for SolidityParser {
    fn language(&self) -> Language {
        Language::Solidity
    }

    fn extensions(&self) -> &[&str] {
        &["sol"]
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

/// Walk backwards through preceding siblings collecting NatSpec (`///` or `/** */`)
/// or consecutive line comments.
fn get_doc_comment(source: &[u8], node: Node) -> Option<String> {
    let mut line_comments: Vec<String> = Vec::new();
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "comment" => {
                let text = node_text(source, p);
                if text.starts_with("/**") {
                    // NatSpec block comment — takes priority over line comments.
                    return Some(text.to_string());
                }
                if text.starts_with("///") || text.starts_with("//") {
                    line_comments.push(text.to_string());
                    prev = p.prev_sibling();
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    if line_comments.is_empty() {
        return None;
    }
    line_comments.reverse();
    Some(line_comments.join("\n"))
}

/// Returns `(byte_end, line_end)` for a contract/library/interface node.
///
/// Trims to the header line when the body contains function definitions with
/// bodies, leaving only the declaration visible. Interface/library/contract
/// bodies that contain only signatures, events, errors, or state variables
/// are returned at full extent.
fn contract_symbol_end(source: &[u8], node: Node) -> (usize, u32) {
    let full = (node.end_byte(), node.end_position().row as u32 + 1);

    let Some(body) = node.child_by_field_name("body") else {
        return full;
    };

    let has_function_bodies = {
        let mut cursor = body.walk();
        let result = body.children(&mut cursor).any(|child| {
            matches!(
                child.kind(),
                "function_definition"
                    | "constructor_definition"
                    | "modifier_definition"
                    | "fallback_receive_definition"
            ) && child.child_by_field_name("body").is_some()
        });
        result
    };

    if !has_function_bodies {
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

fn push_contract_symbol(
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
    let (byte_end, line_end) = contract_symbol_end(source, node);
    symbols.push(Symbol {
        id,
        name: name.to_string(),
        qualified: name.to_string(),
        kind,
        language: Language::Solidity,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end,
        line_start: start_pos.row as u32 + 1,
        line_end,
        signature,
        doc,
    });
}

fn push_member_symbol(
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
        language: Language::Solidity,
        file: Arc::new(path.to_path_buf()),
        byte_start: node.start_byte(),
        byte_end: node.end_byte(),
        line_start: start_pos.row as u32 + 1,
        line_end: end_pos.row as u32 + 1,
        signature,
        doc,
    });
}

fn extract_from_node(
    source: &[u8],
    node: Node,
    path: &std::path::Path,
    contract_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "contract_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_contract_symbol(source, node, path, &name, SymbolKind::Class, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "interface_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_contract_symbol(source, node, path, &name, SymbolKind::Interface, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "library_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                push_contract_symbol(source, node, path, &name, SymbolKind::Class, symbols);
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        extract_from_node(source, child, path, Some(&name), symbols);
                    }
                }
            }
        }
        "struct_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                let qualified = match contract_name {
                    Some(cn) => format!("{cn}::{name}"),
                    None => name.clone(),
                };
                let id = make_symbol_id(path, &qualified, &SymbolKind::Struct);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();
                symbols.push(Symbol {
                    id,
                    name,
                    qualified,
                    kind: SymbolKind::Struct,
                    language: Language::Solidity,
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
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(source, name_node).to_string();
                let qualified = match contract_name {
                    Some(cn) => format!("{cn}::{name}"),
                    None => name.clone(),
                };
                let id = make_symbol_id(path, &qualified, &SymbolKind::Enum);
                let doc = get_doc_comment(source, node);
                let signature = get_signature(source, node);
                let start_pos = node.start_position();
                let end_pos = node.end_position();
                symbols.push(Symbol {
                    id,
                    name,
                    qualified,
                    kind: SymbolKind::Enum,
                    language: Language::Solidity,
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
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let fn_name = node_text(source, name_node).to_string();
                let (qualified, kind) = match contract_name {
                    Some(cn) => (format!("{cn}::{fn_name}"), SymbolKind::Method),
                    None => (fn_name.clone(), SymbolKind::Function),
                };
                push_member_symbol(source, node, path, fn_name, qualified, kind, symbols);
            }
        }
        "constructor_definition" => {
            let name = "constructor".to_string();
            let (qualified, kind) = match contract_name {
                Some(cn) => (format!("{cn}::constructor"), SymbolKind::Method),
                None => (name.clone(), SymbolKind::Function),
            };
            push_member_symbol(source, node, path, name, qualified, kind, symbols);
        }
        "modifier_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let mod_name = node_text(source, name_node).to_string();
                let (qualified, kind) = match contract_name {
                    Some(cn) => (format!("{cn}::{mod_name}"), SymbolKind::Method),
                    None => (mod_name.clone(), SymbolKind::Function),
                };
                push_member_symbol(source, node, path, mod_name, qualified, kind, symbols);
            }
        }
        "event_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let ev_name = node_text(source, name_node).to_string();
                let qualified = match contract_name {
                    Some(cn) => format!("{cn}::{ev_name}"),
                    None => ev_name.clone(),
                };
                push_member_symbol(
                    source,
                    node,
                    path,
                    ev_name,
                    qualified,
                    SymbolKind::Function,
                    symbols,
                );
            }
        }
        "error_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let err_name = node_text(source, name_node).to_string();
                let qualified = match contract_name {
                    Some(cn) => format!("{cn}::{err_name}"),
                    None => err_name.clone(),
                };
                push_member_symbol(
                    source,
                    node,
                    path,
                    err_name,
                    qualified,
                    SymbolKind::Function,
                    symbols,
                );
            }
        }
        // Don't recurse into function/modifier bodies.
        "function_body" => {}
        _ => {
            // At the top level, recurse to catch any wrapped declarations.
            if contract_name.is_none() {
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
            .set_language(&tree_sitter_solidity::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        SolidityParser.extract_symbols(source, &tree, Path::new("test.sol"))
    }

    #[test]
    fn test_extract_contract() {
        let source = b"contract Foo {}";
        let symbols = parse_and_extract(source);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Foo");
        assert!(matches!(symbols[0].kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_interface() {
        let source =
            b"interface IERC20 { function totalSupply() external view returns (uint256); }";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "IERC20").unwrap();
        assert!(matches!(iface.kind, SymbolKind::Interface));
    }

    #[test]
    fn test_extract_library() {
        let source = b"library SafeMath { function add(uint a, uint b) internal pure returns (uint) { return a + b; } }";
        let symbols = parse_and_extract(source);
        let lib = symbols.iter().find(|s| s.name == "SafeMath").unwrap();
        assert!(matches!(lib.kind, SymbolKind::Class));
    }

    #[test]
    fn test_extract_function() {
        let source =
            b"contract Token { function transfer(address to, uint amount) public returns (bool) { return true; } }";
        let symbols = parse_and_extract(source);
        let method = symbols.iter().find(|s| s.name == "transfer").unwrap();
        assert!(matches!(method.kind, SymbolKind::Method));
        assert_eq!(method.qualified, "Token::transfer");
    }

    #[test]
    fn test_extract_constructor() {
        let source = b"contract Token { constructor(uint _supply) { } }";
        let symbols = parse_and_extract(source);
        let ctor = symbols.iter().find(|s| s.name == "constructor").unwrap();
        assert!(matches!(ctor.kind, SymbolKind::Method));
        assert_eq!(ctor.qualified, "Token::constructor");
    }

    #[test]
    fn test_extract_modifier() {
        let source =
            b"contract Owned { modifier onlyOwner() { require(msg.sender == owner); _; } }";
        let symbols = parse_and_extract(source);
        let m = symbols.iter().find(|s| s.name == "onlyOwner").unwrap();
        assert!(matches!(m.kind, SymbolKind::Method));
        assert_eq!(m.qualified, "Owned::onlyOwner");
    }

    #[test]
    fn test_extract_event() {
        let source = b"contract Token { event Transfer(address indexed from, address indexed to, uint amount); }";
        let symbols = parse_and_extract(source);
        let ev = symbols.iter().find(|s| s.name == "Transfer").unwrap();
        assert_eq!(ev.qualified, "Token::Transfer");
    }

    #[test]
    fn test_extract_error() {
        let source =
            b"contract Token { error InsufficientBalance(uint available, uint required); }";
        let symbols = parse_and_extract(source);
        let err = symbols
            .iter()
            .find(|s| s.name == "InsufficientBalance")
            .unwrap();
        assert_eq!(err.qualified, "Token::InsufficientBalance");
    }

    #[test]
    fn test_extract_struct() {
        let source = b"contract Vault { struct Position { uint256 amount; uint256 debt; } }";
        let symbols = parse_and_extract(source);
        let s = symbols.iter().find(|s| s.name == "Position").unwrap();
        assert!(matches!(s.kind, SymbolKind::Struct));
        assert_eq!(s.qualified, "Vault::Position");
    }

    #[test]
    fn test_extract_enum() {
        let source = b"contract Vault { enum Status { Active, Liquidated } }";
        let symbols = parse_and_extract(source);
        let e = symbols.iter().find(|s| s.name == "Status").unwrap();
        assert!(matches!(e.kind, SymbolKind::Enum));
        assert_eq!(e.qualified, "Vault::Status");
    }

    #[test]
    fn test_natspec_extracted() {
        let source = b"/// @notice A basic token\ncontract Token {}";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("basic token"), "doc={doc}");
    }

    #[test]
    fn test_block_natspec_extracted() {
        let source = b"/** @dev A storage contract */\ncontract Storage {}";
        let symbols = parse_and_extract(source);
        let doc = symbols[0].doc.as_deref().unwrap_or("");
        assert!(doc.contains("storage contract"), "doc={doc}");
    }

    #[test]
    fn test_contract_body_trimmed_when_has_functions() {
        let source =
            b"contract Foo {\n    function bar() public {\n        doSomething();\n    }\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Foo").unwrap();
        assert_eq!(
            cls.line_start, cls.line_end,
            "contract should be trimmed to header line"
        );
    }

    #[test]
    fn test_interface_not_trimmed() {
        let source =
            b"interface IFoo {\n    function bar() external;\n    function baz() external;\n}";
        let symbols = parse_and_extract(source);
        let iface = symbols.iter().find(|s| s.name == "IFoo").unwrap();
        assert!(
            iface.line_end > iface.line_start,
            "interface with no bodies should not be trimmed"
        );
    }

    #[test]
    fn test_signature_is_first_line() {
        let source = b"contract Token {\n    uint256 supply;\n}";
        let symbols = parse_and_extract(source);
        let cls = symbols.iter().find(|s| s.name == "Token").unwrap();
        let sig = cls.signature.as_deref().unwrap_or("");
        assert_eq!(sig, "contract Token {");
    }

    #[test]
    fn test_multiple_contracts() {
        let source = b"contract Foo {}\ncontract Bar {}";
        let symbols = parse_and_extract(source);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Foo"));
        assert!(names.contains(&"Bar"));
    }

    #[test]
    fn test_top_level_struct_and_enum() {
        let source = b"struct Point { uint x; uint y; }\nenum Color { Red, Green, Blue }";
        let symbols = parse_and_extract(source);
        let st = symbols.iter().find(|s| s.name == "Point").unwrap();
        assert!(matches!(st.kind, SymbolKind::Struct));
        assert_eq!(st.qualified, "Point");
        let en = symbols.iter().find(|s| s.name == "Color").unwrap();
        assert!(matches!(en.kind, SymbolKind::Enum));
        assert_eq!(en.qualified, "Color");
    }
}
