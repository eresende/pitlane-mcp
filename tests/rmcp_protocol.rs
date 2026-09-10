use std::{
    io::Write,
    process::{Command, Stdio},
};

use serde_json::{json, Value};

/// Send a sequence of NDJSON MCP messages and return all JSON responses.
fn send_messages(messages: &[Value]) -> Vec<Value> {
    send_messages_with_env(messages, &[])
}

/// Send a sequence of NDJSON MCP messages with extra environment variables
/// for the spawned server, and return all JSON responses.
fn send_messages_with_env(messages: &[Value], envs: &[(&str, &str)]) -> Vec<Value> {
    let input = messages
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let mut command = Command::new(env!("CARGO_BIN_EXE_pitlane-mcp"));
    command
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn pitlane-mcp server");
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
        .collect()
}

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

    send_messages(&[initialize, initialized, list_tools])
        .into_iter()
        .find(|message| message.get("id") == Some(&json!(2)))
        .and_then(|message| message.get("result").cloned())
        .expect("tools/list response")
}

#[test]
fn analyze_changes_is_public_with_required_revision_schema() {
    let result = tools_list_result("2026-07-28");
    let tool = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "analyze_changes")
        .expect("public analyze_changes tool");
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&json!("project")));
    assert!(required.contains(&json!("base_ref")));
    assert!(!required.contains(&json!("include_working_tree")));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
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

#[test]
fn get_index_changes_is_public_with_project_required() {
    let result = tools_list_result("2026-07-28");
    let tool = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "get_index_changes")
        .expect("public get_index_changes tool");
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&json!("project")));
    assert!(!required.contains(&json!("since_revision")));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[test]
fn doctor_is_public_with_project_required() {
    let result = tools_list_result("2026-07-28");
    let tool = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "doctor")
        .expect("public doctor tool");
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(required.contains(&json!("project")));
    assert!(!required.contains(&json!("repair")));
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[test]
fn investigate_exposes_budget_parameters() {
    let result = tools_list_result("2026-07-28");
    let tool = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "investigate")
        .expect("public investigate tool");
    let props = tool["inputSchema"]["properties"].as_object().unwrap();
    assert!(props.contains_key("token_budget"));
    assert!(props.contains_key("include_tests"));
    let required = tool["inputSchema"]["required"].as_array().unwrap();
    assert!(!required.contains(&json!("token_budget")));
    assert!(!required.contains(&json!("include_tests")));
}

/// End-to-end: calling get_index_stats without the project-path parameter
/// returns a friendly error that names both accepted spellings and does not
/// include serde's "at line 1 column N" noise.
#[test]
fn missing_project_path_returns_friendly_error() {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-protocol-test", "version": "1" }
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    // Call get_index_stats with an empty arguments object (missing project).
    let call_tool = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "get_index_stats",
            "arguments": {}
        }
    });

    let responses = send_messages(&[initialize, initialized, call_tool]);
    let tool_response = responses
        .iter()
        .find(|m| m.get("id") == Some(&json!(3)))
        .expect("tools/call response");

    // The response should be an error result (isError: true).
    assert_eq!(tool_response["result"]["isError"], json!(true));
    let content = &tool_response["result"]["content"][0];
    let text = content["text"]
        .as_str()
        .expect("error result includes text content");

    // Acceptance criteria from issue #110:
    // 1. Error names the parameter and both accepted spellings.
    assert!(
        text.contains("`project`") || text.contains("'project'"),
        "error should name canonical field 'project': {text}"
    );
    assert!(
        text.contains("`path`") || text.contains("'path'"),
        "error should name alias field 'path': {text}"
    );

    // 2. No serde line/column noise.
    assert!(
        !text.contains("line 1 column"),
        "error must not contain serde line/column noise: {text}"
    );

    // 3. Includes an example.
    assert!(
        text.contains("Example:"),
        "error should include a valid example: {text}"
    );
}

/// End-to-end: calling ensure_project_ready without the path parameter
/// returns a friendly error (canonical field is `path` for this tool).
#[test]
fn missing_path_on_ensure_project_ready_returns_friendly_error() {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-protocol-test", "version": "1" }
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let call_tool = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "ensure_project_ready",
            "arguments": {}
        }
    });

    let responses = send_messages(&[initialize, initialized, call_tool]);
    let tool_response = responses
        .iter()
        .find(|m| m.get("id") == Some(&json!(4)))
        .expect("tools/call response");

    let content = &tool_response["result"]["content"][0];
    let text = content["text"]
        .as_str()
        .expect("error result includes text content");

    // ensure_project_ready uses `path` as canonical, `project` as alias.
    assert_eq!(tool_response["result"]["isError"], json!(true));
    assert!(
        text.contains("`path`") || text.contains("'path'"),
        "error should name canonical field 'path': {text}"
    );
    assert!(
        text.contains("`project`") || text.contains("'project'"),
        "error should name alias field 'project': {text}"
    );
    assert!(
        !text.contains("line 1 column"),
        "error must not contain serde line/column noise: {text}"
    );
}

/// Data-driven regression guard (issue #114): every tool whose schema
/// requires a project or path parameter must return the friendly error
/// when called with empty arguments. A new tool with a required project
/// path that is not wrapped will fail this test.
#[test]
fn every_project_path_tool_emits_friendly_error_for_empty_arguments() {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-protocol-test", "version": "1" }
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

    // Deserialization fails before any handler side effects, so it is safe
    // to call every tool (including mutating ones) with empty arguments.
    let tools = {
        let responses = send_messages_with_env(
            &[initialize.clone(), initialized.clone(), list_tools.clone()],
            &[("PITLANE_MCP_TOOL_TIER", "all")],
        );
        responses
            .iter()
            .find(|m| m.get("id") == Some(&json!(2)))
            .and_then(|m| m.get("result").and_then(|r| r.get("tools")).cloned())
            .expect("tools/list response")
            .as_array()
            .expect("tools array")
            .clone()
    };

    let mut messages = vec![initialize, initialized];
    let mut id = 3;
    let mut targets = Vec::new();
    for tool in &tools {
        let name = tool["name"].as_str().expect("tool name").to_string();
        let required = tool["inputSchema"]["required"]
            .as_array()
            .map(|r| r.iter().any(|v| v == "project" || v == "path"))
            .unwrap_or(false);
        if required {
            targets.push(name.clone());
            messages.push(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": name, "arguments": {} }
            }));
            id += 1;
        }
    }
    assert!(
        !targets.is_empty(),
        "expected at least one tool with a required project/path field"
    );

    let responses = send_messages_with_env(&messages, &[("PITLANE_MCP_TOOL_TIER", "all")]);
    for (offset, name) in targets.iter().enumerate() {
        let tool_response = responses
            .iter()
            .find(|m| m.get("id") == Some(&json!(3 + offset)))
            .unwrap_or_else(|| panic!("missing tools/call response for {name}"));
        assert_eq!(
            tool_response["result"]["isError"],
            json!(true),
            "expected isError=true for {name}"
        );
        let text = tool_response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("missing error text for {name}"));
        assert!(
            text.contains("`project`") || text.contains("`path`"),
            "friendly error for {name} should name the project-path field: {text}"
        );
        assert!(
            text.contains("Example:"),
            "friendly error for {name} should include an example: {text}"
        );
        assert!(
            !text.contains("line 1 column"),
            "friendly error for {name} must not contain serde line/column noise: {text}"
        );
    }
}

/// Issue #112: get_project_outline is the one tool where `path` is NOT an
/// alias for the project root (it is the subdirectory filter), so the
/// friendly error must name only `project` and explain the caveat.
#[test]
fn missing_project_on_get_project_outline_returns_friendly_canonical_only_error() {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": { "name": "rmcp-protocol-test", "version": "1" }
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let call_tool = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "get_project_outline",
            "arguments": {}
        }
    });

    let responses = send_messages_with_env(
        &[initialize, initialized, call_tool],
        &[("PITLANE_MCP_TOOL_TIER", "all")],
    );
    let tool_response = responses
        .iter()
        .find(|m| m.get("id") == Some(&json!(5)))
        .expect("tools/call response");

    assert_eq!(tool_response["result"]["isError"], json!(true));
    let text = tool_response["result"]["content"][0]["text"]
        .as_str()
        .expect("error result includes text content");

    // Names the canonical field and explains the `path` caveat.
    assert!(
        text.contains("`project`"),
        "error should name canonical field 'project': {text}"
    );
    assert!(
        text.contains("subdirectory filter"),
        "error should explain the `path` caveat: {text}"
    );
    assert!(
        text.contains("Example:") && text.contains("/path/to/project"),
        "error should include a valid example: {text}"
    );
    assert!(
        !text.contains("line 1 column"),
        "error must not contain serde line/column noise: {text}"
    );
}
