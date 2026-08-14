use std::{
    io::Write,
    process::{Command, Stdio},
};

use serde_json::{json, Value};

fn tools_list_result(protocol_version: &str) -> Value {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {
                "name": "rmcp-protocol-test",
                "version": "1"
            }
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let list_tools = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let input = format!("{initialize}\n{initialized}\n{list_tools}\n");

    let mut child = Command::new(env!("CARGO_BIN_EXE_pitlane-mcp"))
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pitlane-mcp server");
    child
        .stdin
        .take()
        .expect("server stdin")
        .write_all(input.as_bytes())
        .expect("write MCP requests");

    let output = child.wait_with_output().expect("wait for pitlane-mcp");
    assert!(
        output.status.success(),
        "pitlane-mcp exited unsuccessfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("server output is UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message.get("id") == Some(&json!(2)))
        .and_then(|message| message.get("result").cloned())
        .expect("tools/list response")
}

#[test]
fn legacy_tools_list_omits_2026_result_fields() {
    let result = tools_list_result("2025-11-25");

    assert!(result.get("resultType").is_none());
    assert!(result.get("ttlMs").is_none());
    assert!(result.get("cacheScope").is_none());
    assert!(result["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty()));
}

#[test]
fn protocol_2026_tools_list_includes_required_result_fields() {
    let result = tools_list_result("2026-07-28");

    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["ttlMs"], 0);
    assert_eq!(result["cacheScope"], "public");
    assert!(result["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty()));
}
